use avian3d::prelude::{Collider, CollisionMargin, RigidBody};
use bevy::{
    asset::RenderAssetUsages,
    camera::visibility::RenderLayers,
    math::{bounding::Aabb3d, prelude::*},
    mesh::{Indices, PrimitiveTopology},
    pbr::Lightmap,
    platform::collections::{HashMap, hash_map::Entry},
    prelude::*,
};
use itertools::Itertools;
use qbsp::data::LightmapStyle;

use crate::{
    BspAsset, SOURCE_TO_BEVY,
    entities::{BspBrushEntityMesh, BspWorldspawnMesh},
    visdata::{CalculateVisleaf, VisClusters, VisRoot},
};

#[derive(Clone)]
pub struct BspMesh {
    positions: Vec<Vec3>,
    normals: Vec<Vec3>,
    indices: Vec<u16>,
    texture_uvs: Vec<Vec2>,
    lightmap_uvs: Vec<Vec2>,
}

impl BspMesh {
    fn collider(&self) -> Collider {
        let indices = self
            .indices
            .as_chunks::<3>()
            .0
            .iter()
            .map(|idxs| idxs.map(|i| i as u32))
            .collect::<Vec<_>>();

        Collider::trimesh(self.positions.clone(), indices)
    }

    fn merge(&mut self, other: &Self) {
        let idx_offset: u16 = self
            .positions
            .len()
            .try_into()
            .expect("Vertices out of range for u16");

        self.positions.extend_from_slice(&other.positions);
        self.normals.extend_from_slice(&other.normals);
        self.texture_uvs.extend_from_slice(&other.texture_uvs);
        self.lightmap_uvs.extend_from_slice(&other.lightmap_uvs);

        self.indices.extend(other.indices.iter().map(|i| {
            i.checked_add(idx_offset)
                .expect("Idx overflowed during merge")
        }));
    }

    /// Bounds in the mesh's own (BSP-local) coordinates.
    fn aabb(&self) -> Option<Aabb3d> {
        let (min, max) = self.positions.iter().fold(
            (Vec3::INFINITY, Vec3::NEG_INFINITY),
            |(min, max), &position| (min.min(position), max.max(position)),
        );

        (min.x <= max.x).then(|| Aabb3d::from_min_max(min, max))
    }
}

impl BspMesh {
    pub fn into_mesh(self, usages: RenderAssetUsages) -> Mesh {
        Mesh::new(PrimitiveTopology::TriangleList, usages)
            .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, self.texture_uvs)
            .with_inserted_attribute(Mesh::ATTRIBUTE_UV_1, self.lightmap_uvs)
            .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, self.normals)
            .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, self.positions)
            .with_inserted_indices(Indices::U16(self.indices))
    }
}

fn mesh_from_face(
    model_origin: Vec3,
    face: &vbsp::Handle<'_, vbsp::Face>,
    lightmap_uv_rect: &Rect,
) -> Option<BspMesh> {
    if !face.is_visible() {
        return None;
    }

    let (texture_uvs, lightmap_uvs, positions): (Vec<Vec2>, Vec<Vec2>, Vec<Vec3>) = face
        .vertex_positions()
        .zip(face.lightmap_uvs())
        .map(|(position, lightmap_uv)| {
            let lightmap_uv = Vec2::new(lightmap_uv.x, lightmap_uv.y);

            let lm_size = lightmap_uv_rect.size();
            let lm_size = Vec2::new(lm_size.x, lm_size.y);

            let uv = face.texture().uv(position);
            let uv = Vec2::new(uv.x, uv.y);

            let position = Vec3::new(position.x, position.y, position.z);

            (
                uv,
                lightmap_uv_rect.min + lm_size * lightmap_uv,
                model_origin + position,
            )
        })
        .multiunzip();

    let indices: Vec<_> = face.triangulate_indices().map(|i| i as _).collect();
    let normal = face.normal();
    let normal = Vec3::new(normal.x, normal.y, normal.z);
    let normals = vec![normal; positions.len()];

    Some(BspMesh {
        positions,
        normals,
        indices,
        texture_uvs,
        lightmap_uvs,
    })
}

pub trait FaceSpawner: Default {
    /// Whether `spawn_worldspawn` should compute per-face PVS cluster
    /// membership (and build a [`VisRoot`]) for this spawner.
    const NEEDS_CLUSTERS: bool;

    /// Extra components for loose meshes spawned outside `spawn_worldspawn`
    /// (static props, brush-entity models) so they participate in PVS culling.
    fn orphaned_face_bundle() -> impl Bundle {}

    /// `clusters` is the face's PVS membership (empty = unknown/everywhere;
    /// always empty when [`Self::NEEDS_CLUSTERS`] is false).
    fn merge_face_mesh(
        &mut self,
        clusters: Vec<u32>,
        texture: String,
        lightmap: Option<Handle<Image>>,
        mesh: BspMesh,
    );

    fn finish(self) -> impl Iterator<Item = (Box<[u32]>, String, Option<Handle<Image>>, BspMesh)>;
}

/// Groups world faces by the first cluster they appear in, so the PVS can
/// cull them per area. The merged mesh's membership is the union of its
/// faces' clusters — conservative (a mesh renders if *any* of its faces
/// could be seen), never wrong.
#[derive(Default)]
pub struct VisclusterFaceSpawner {
    faces: HashMap<(Option<u32>, String, Option<Handle<Image>>), (BspMesh, Vec<u32>)>,
}

impl FaceSpawner for VisclusterFaceSpawner {
    const NEEDS_CLUSTERS: bool = true;

    fn orphaned_face_bundle() -> impl Bundle {
        (CalculateVisleaf, RenderLayers::none())
    }

    fn merge_face_mesh(
        &mut self,
        clusters: Vec<u32>,
        texture: String,
        lightmap: Option<Handle<Image>>,
        mesh: BspMesh,
    ) {
        let primary = clusters.first().copied();

        match self.faces.entry((primary, texture, lightmap)) {
            Entry::Occupied(mut entry) => {
                let (existing_mesh, existing_clusters) = entry.get_mut();
                existing_mesh.merge(&mesh);
                for cluster in clusters {
                    if !existing_clusters.contains(&cluster) {
                        existing_clusters.push(cluster);
                    }
                }
            }
            Entry::Vacant(entry) => {
                entry.insert((mesh, clusters));
            }
        }
    }

    fn finish(self) -> impl Iterator<Item = (Box<[u32]>, String, Option<Handle<Image>>, BspMesh)> {
        self.faces
            .into_iter()
            .map(|((_, texture_name, lightmap), (mesh, clusters))| {
                (clusters.into_boxed_slice(), texture_name, lightmap, mesh)
            })
    }
}

#[derive(Default)]
pub struct GlobalFaceSpawner {
    faces: HashMap<(String, Option<Handle<Image>>), BspMesh>,
}

impl FaceSpawner for GlobalFaceSpawner {
    const NEEDS_CLUSTERS: bool = false;

    fn merge_face_mesh(
        &mut self,
        _: Vec<u32>,
        texture: String,
        lightmap: Option<Handle<Image>>,
        mesh: BspMesh,
    ) {
        self.faces
            .entry((texture, lightmap))
            .and_modify(|existing| existing.merge(&mesh))
            .or_insert(mesh);
    }

    fn finish(self) -> impl Iterator<Item = (Box<[u32]>, String, Option<Handle<Image>>, BspMesh)> {
        self.faces
            .into_iter()
            .map(|((texture_name, lightmap), mesh)| (Box::default(), texture_name, lightmap, mesh))
    }
}

/// Whether a face's texture is one of Hammer's editor tools
fn is_tool_texture(texture_name: &str) -> bool {
    texture_name.contains("tools/")
}

pub fn spawn_worldspawn<FS: FaceSpawner>(
    commands: &mut Commands,
    map_root: Entity,
    bsp_asset: &BspAsset,
    meshes: &mut Assets<Mesh>,
    model: vbsp::Handle<'_, vbsp::Model>,
    styles_to_image: &HashMap<LightmapStyle, (Handle<Image>, UVec2)>,
    face_to_lightmap_uv: &HashMap<u32, vbsp::Rect>,
    usages: RenderAssetUsages,
) -> Entity {
    struct ConstructedFace {
        texture_name: String,
        mesh: BspMesh,
        lightmap: Option<Handle<Image>>,
    }

    let first_model_face = model.first_face;

    let faces = model
        .faces_with_id()
        .map(|(face_idx, face)| {
            let lightmap_handle = styles_to_image.get(&LightmapStyle(face.styles[0]));
            let lightmap_size = lightmap_handle.as_ref().map(|(_, size)| size.as_vec2());

            let lightmap_uv_rect = lightmap_size
                .and_then(|lightmap_size| {
                    let lightmap_rect = face_to_lightmap_uv.get(&face_idx)?;

                    let min = UVec2::new(lightmap_rect.x, lightmap_rect.y).as_vec2();
                    let size = UVec2::new(lightmap_rect.width, lightmap_rect.height).as_vec2();

                    Some(Rect {
                        min: min / lightmap_size,
                        max: (min + size) / lightmap_size,
                    })
                })
                .unwrap_or_default();

            let mesh = mesh_from_face(Vec3::ZERO, &face, &lightmap_uv_rect)?;
            let texture_name = face.texture().name().to_ascii_lowercase();

            Some(ConstructedFace {
                texture_name,
                mesh,
                lightmap: lightmap_handle.map(|(handle, _)| handle.clone()),
            })
        })
        .collect::<Vec<_>>();

    let root_ent = commands
        .spawn((
            ChildOf(map_root),
            Visibility::Visible,
            Transform::from_matrix(SOURCE_TO_BEVY.into()),
        ))
        .id();

    // Per-face PVS membership: primarily from the leaf-face lists; faces the
    // leaves don't reference (displacements, node-parented faces) fall back
    // to overlapping their bounds against the leaves. All in BSP-local
    // coordinates, computed once here — nothing about the tree or the PVS
    // becomes entities.
    let vis_root = FS::NEEDS_CLUSTERS.then(|| VisRoot::from_bsp(&bsp_asset.bsp));
    let mut face_clusters = vec![Vec::<u32>::new(); faces.len()];

    if let Some(vis_root) = &vis_root {
        let bsp = &bsp_asset.bsp;
        let model_face_range = first_model_face..first_model_face + model.face_count;

        for leaf in bsp.leaves.iter() {
            let Ok(cluster) = u32::try_from(leaf.cluster) else {
                continue;
            };

            let leaf_face_range = leaf.first_leaf_face as usize
                ..(leaf.first_leaf_face + leaf.leaf_face_count) as usize;

            for leaf_face in bsp.leaf_faces.get(leaf_face_range).unwrap_or_default() {
                let face_idx = leaf_face.face as u32;
                if !model_face_range.contains(&face_idx) {
                    continue;
                }

                let clusters = &mut face_clusters[(face_idx - first_model_face) as usize];
                if !clusters.contains(&cluster) {
                    clusters.push(cluster);
                }
            }
        }

        for (face, clusters) in faces.iter().zip(&mut face_clusters) {
            let Some(face) = face else { continue };

            if clusters.is_empty()
                && let Some(aabb) = face.mesh.aabb()
            {
                vis_root.clusters_for_aabb(aabb, clusters);
            }
        }
    }

    let mut face_spawner = FS::default();

    for (face, clusters) in faces.into_iter().zip(face_clusters) {
        let Some(face) = face else {
            continue;
        };

        face_spawner.merge_face_mesh(clusters, face.texture_name, face.lightmap, face.mesh);
    }

    for (clusters, texture_name, lightmap, mesh) in face_spawner.finish() {
        let collider = mesh.collider();

        if is_tool_texture(&texture_name) {
            commands.spawn((
                BspWorldspawnMesh {
                    surface_prop: bsp_asset.surface_prop(&texture_name).map(str::to_owned),
                    texture_name: texture_name.clone(),
                },
                CollisionMargin(0.01),
                collider,
                RigidBody::Static,
                ChildOf(root_ent),
            ));
            continue;
        }

        let material = bsp_asset
            .materials
            .get(&texture_name)
            .cloned()
            .unwrap_or_else(|| {
                warn!("No material for BSP model: {texture_name}");
                bsp_asset.default_material.0.clone()
            });

        let mesh_handle = meshes.add(mesh.into_mesh(usages));

        let mut out = commands.spawn((
            BspWorldspawnMesh {
                surface_prop: bsp_asset.surface_prop(&texture_name).map(str::to_owned),
                texture_name: texture_name.clone(),
            },
            CollisionMargin(0.01),
            collider,
            RigidBody::Static,
            Mesh3d(mesh_handle),
            MeshMaterial3d(material),
            ChildOf(root_ent),
        ));

        if FS::NEEDS_CLUSTERS {
            out.insert((
                RenderLayers::none(),
                VisClusters {
                    root: root_ent,
                    clusters,
                },
            ));
        }

        if let Some(lightmap) = lightmap {
            out.insert(Lightmap {
                image: lightmap.clone(),
                ..Default::default()
            });
        }
    }

    if let Some(vis_root) = vis_root {
        commands.entity(root_ent).insert(vis_root);
    }

    root_ent
}

pub fn spawn_bsp_model<FS: FaceSpawner>(
    commands: &mut Commands,
    map_root: Entity,
    bsp_asset: &BspAsset,
    meshes: &mut Assets<Mesh>,
    model: vbsp::Handle<'_, vbsp::Model>,
    styles_to_image: &HashMap<LightmapStyle, (Handle<Image>, UVec2)>,
    face_to_lightmap_uv: &HashMap<u32, vbsp::Rect>,
    transform: Transform,
    classname: &str,
    model_index: usize,
    usages: RenderAssetUsages,
) {
    let mut meshes_to_spawn: HashMap<(String, Option<Handle<Image>>), (Mesh, Option<Lightmap>)> =
        HashMap::new();

    for (face_idx, face) in model.faces_with_id() {
        let lightmap_handle = styles_to_image.get(&LightmapStyle(face.styles[0])).cloned();
        let Some(lightmap_size) = lightmap_handle.as_ref().map(|(_, size)| size.as_vec2()) else {
            continue;
        };

        let lightmap_rect = face_to_lightmap_uv[&face_idx];

        let min = UVec2::new(lightmap_rect.x, lightmap_rect.y).as_vec2();
        let size = UVec2::new(lightmap_rect.width, lightmap_rect.height).as_vec2();
        let lightmap_uv_rect = Rect {
            min: min / lightmap_size,
            max: (min + size) / lightmap_size,
        };

        let origin = Vec3::new(model.origin.x, model.origin.y, model.origin.z);

        let Some(mesh) = mesh_from_face(origin, &face, &lightmap_uv_rect) else {
            continue;
        };

        let mesh = mesh.into_mesh(usages);

        match meshes_to_spawn.entry((
            face.texture().name().to_ascii_lowercase(),
            lightmap_handle.as_ref().map(|(handle, _)| handle.clone()),
        )) {
            Entry::Occupied(mut entry) => {
                let entry = entry.get_mut();
                entry.0.merge(&mesh).unwrap();
            }
            Entry::Vacant(entry) => {
                let lightmap = lightmap_handle.map(|(image, _)| Lightmap {
                    image: image.clone(),
                    bicubic_sampling: true,
                    ..Default::default()
                });

                entry.insert((mesh, lightmap));
            }
        }
    }

    for ((texture_name, _), (mesh, lightmap)) in meshes_to_spawn {
        if is_tool_texture(&texture_name) {
            continue;
        }
        let material = bsp_asset
            .materials
            .get(&texture_name)
            .cloned()
            .unwrap_or_else(|| {
                warn!("No material for BSP model: {}", texture_name);
                bsp_asset.default_material.0.clone()
            });
        debug!("Spawning model texture={texture_name}");
        let collider = Collider::convex_decomposition_from_mesh(&mesh);
        let mesh_handle = meshes.add(mesh);

        let mut entity = commands.spawn((
            ChildOf(map_root),
            BspBrushEntityMesh {
                surface_prop: bsp_asset.surface_prop(&texture_name).map(str::to_owned),
                texture_name: texture_name.clone(),
                model_index,
                classname: classname.to_owned(),
            },
            Mesh3d(mesh_handle),
            MeshMaterial3d(material.clone()),
            transform,
            FS::orphaned_face_bundle(),
        ));

        if let Some(lightmap) = lightmap {
            entity.insert(lightmap);
        }

        if let Some(collider) = collider {
            entity.insert((collider, RigidBody::Static));
        } else {
            warn!("No collider for texture: {}", texture_name);
        }
    }
}

pub fn spawn_mdl_model(
    bsp_asset: &BspAsset,
    model: &vmdl::Model,
    usages: RenderAssetUsages,
) -> impl Iterator<Item = (Mesh, Handle<StandardMaterial>)> {
    // TODO: Handle bones, since many models won't render with the correct
    // transform if they aren't handled.
    model
        .meshes()
        .zip(model.textures())
        .map(move |(mdl_mesh, texture_info)| {
            let (vertices, normals, uvs): (Vec<_>, Vec<_>, Vec<_>) = mdl_mesh
                .vertices()
                .map(|v| {
                    (
                        Vec3::new(v.position.x, v.position.y, v.position.z),
                        Vec3::new(v.normal.x, v.normal.y, v.normal.z),
                        v.texture_coordinates,
                    )
                })
                .multiunzip();

            let indices = mdl_mesh
                .vertex_strip_indices()
                .map(|idx| idx.try_into().expect("mdl index overflow"))
                .collect::<Vec<u16>>();

            let mesh = Mesh::new(PrimitiveTopology::TriangleList, usages)
                .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, vertices)
                .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uvs)
                .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, normals)
                .with_inserted_indices(Indices::U16(indices));

            let texture_path = texture_info.name.to_ascii_lowercase();
            let material = bsp_asset.materials.get(&texture_path).unwrap_or_else(|| {
                warn!("No material for MDL model: {:?}", texture_info);
                &bsp_asset.default_material.0
            });

            (mesh, material.clone())
        })
}
