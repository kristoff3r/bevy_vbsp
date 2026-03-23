pub mod info_player;

use std::collections::{HashMap, hash_map::Entry};

use avian3d::prelude::{Collider, CollisionMargin, RigidBody};
use bevy::{
    asset::RenderAssetUsages,
    camera::{primitives::HalfSpace, visibility::RenderLayers},
    ecs::entity::{EntityHashMap, EntityHashSet},
    mesh::{Indices, PrimitiveTopology},
    pbr::Lightmap,
    prelude::*,
};
use itertools::{Either, Itertools};
use qbsp::data::LightmapStyle;
use serde::Deserialize;

use crate::visdata::{VisChildren, VisClusters, VisTreeElementOf};

use super::{BspAsset, source_to_bevy};

// OLD CODE:
//
// let detail_meshes = root_node.faces_with_id().flat_map(|(face_idx, face)| {
//     if face.displacement().is_some() {
//         dbg!(face_idx);
//     }

//     match cluster_meshes.entry((
//         face.texture().name().to_ascii_lowercase(),
//         lightmap_handle.as_ref().map(|(handle, _)| handle.clone()),
//     )) {
//         Entry::Occupied(mut entry) => {
//             let entry = entry.get_mut();
//             entry.0.merge(&mesh).unwrap();
//         }
//         Entry::Vacant(entry) => {
//             let lightmap = lightmap_handle.map(|(image, _)| Lightmap {
//                 image: image.clone(),
//                 bicubic_sampling: true,
//                 ..Default::default()
//             });

//             entry.insert((mesh, lightmap));
//         }
//     }

//     cluster_meshes
//         .drain()
//         .map(move |((texture_name, _), (mesh, lightmap))| {
//             let collider = Collider::trimesh_from_mesh(&mesh);

//             (texture_name, mesh, collider, lightmap)
//         })
//         .collect::<Vec<_>>()
// });

// for (texture_name, mesh, collider, lightmap) in detail_meshes {
//     if texture_name.contains("tools/") {
//         continue;
//     }
//     let material = bsp_asset
//         .materials
//         .get(&texture_name)
//         .cloned()
//         .unwrap_or_else(|| {
//             warn!("No material for BSP model: {}", texture_name);
//             bsp_asset.default_material.0.clone()
//         });
//     debug!("Spawning model texture={texture_name}");
//     let mesh_handle = meshes.add(mesh);

//     let mut entity = commands.spawn((
//         Mesh3d(mesh_handle),
//         MeshMaterial3d(material.clone()),
//         // transform,
//     ));

//     if let Some(lightmap) = lightmap {
//         entity.insert(lightmap);
//     }

//     if let Some(collider) = collider {
//         entity.insert((collider, RigidBody::Static));
//     } else {
//         warn!("No collider for texture: {}", texture_name);
//     }
// }

// let cluster_meshes = root_node
//     .vis_clusters()
//     .into_iter()
//     .map(|(cluster_idx, cluster)| {
//         let mut collision_vertices = Vec::<Vec3>::new();
//         let mut collision_indices = Vec::<[u32; 3]>::new();

//         for leaf in cluster {
//             for (face_idx, face) in leaf.faces_with_id() {
//                 if face.displacement().is_some() {
//                     dbg!(face_idx);
//                 }

//                 vistree_faces_hashset.insert(face_idx);
//                 let lightmap_handle =
//                     styles_to_image.get(&LightmapStyle(face.styles[0])).cloned();
//                 let Some(lightmap_size) =
//                     lightmap_handle.as_ref().map(|(_, size)| size.as_vec2())
//                 else {
//                     continue;
//                 };

//                 let lightmap_rect = face_to_lightmap_uv[&face_idx];

//                 let min = UVec2::new(lightmap_rect.x, lightmap_rect.y).as_vec2();
//                 let size = UVec2::new(lightmap_rect.width, lightmap_rect.height).as_vec2();
//                 let lightmap_uv_rect = Rect {
//                     min: min / lightmap_size,
//                     max: (min + size) / lightmap_size,
//                 };

//                 let Some(mesh) = mesh_from_face(Vec3::ZERO, &face, &lightmap_uv_rect) else {
//                     continue;
//                 };

//                 collision_indices.extend(
//                     mesh.indices
//                         .as_chunks::<3>()
//                         .0
//                         .iter()
//                         .map(|idxs| idxs.map(|i| collision_vertices.len() as u32 + i as u32)),
//                 );
//                 collision_vertices.extend(mesh.positions.iter().copied());

//                 let mesh = mesh.into();

//                 match cluster_meshes.entry((
//                     face.texture().name().to_ascii_lowercase(),
//                     lightmap_handle.as_ref().map(|(handle, _)| handle.clone()),
//                 )) {
//                     Entry::Occupied(mut entry) => {
//                         let entry = entry.get_mut();
//                         entry.0.merge(&mesh).unwrap();
//                     }
//                     Entry::Vacant(entry) => {
//                         let lightmap = lightmap_handle.map(|(image, _)| Lightmap {
//                             image: image.clone(),
//                             bicubic_sampling: true,
//                             ..Default::default()
//                         });

//                         entry.insert((mesh, lightmap));
//                     }
//                 }
//             }
//         }

//         let collider = if !collision_indices.is_empty() && !collision_vertices.is_empty() {
//             Some(Collider::trimesh(collision_vertices, collision_indices))
//         } else {
//             None
//         };

//         (
//             collider,
//             cluster_meshes
//                 .drain()
//                 .map(move |((texture_name, _), (mesh, lightmap))| {
//                     (cluster_idx, texture_name, mesh, lightmap)
//                 })
//                 .collect::<Vec<_>>(),
//         )
//     });

// for (collider, cluster_meshes) in cluster_meshes {
//     let transform = Transform::from_translation(source_to_bevy(model.origin));

//     if let Some(collider) = collider {
//         commands.spawn((
//             Name::new("Collider"),
//             CollisionMargin(0.01),
//             transform,
//             collider,
//             RigidBody::Static,
//         ));
//     }

//     for (cluster_idx, texture_name, mesh, lightmap) in cluster_meshes {
//         let material = bsp_asset
//             .materials
//             .get(&texture_name)
//             .cloned()
//             .unwrap_or_else(|| {
//                 warn!("No material for BSP model: {}", texture_name);
//                 bsp_asset.default_material.0.clone()
//             });
//         debug!("Spawning model texture={texture_name}");
//         let mesh_handle = meshes.add(mesh);

//         let render_layers = cluster_idx
//             .try_into()
//             .map(|cluster_idx_usize| {
//                 std::iter::once(cluster_idx_usize)
//                     .chain(
//                         bsp_asset
//                             .bsp
//                             .vis_data
//                             .visible_clusters(cluster_idx)
//                             .iter()
//                             .enumerate()
//                             .filter(|(_, visible)| *visible)
//                             .map(|(i, _)| i),
//                     )
//                     // `+ 1` so we have space to add the `0` layer.
//                     .map(|idx| idx + 1)
//                     .chain(std::iter::once(0))
//                     .collect::<RenderLayers>()
//             })
//             .unwrap_or_default();

//         let mut entity = commands.spawn((
//             // render_layers,
//             Mesh3d(mesh_handle),
//             MeshMaterial3d(material.clone()),
//             transform,
//         ));

//         if let Some(lightmap) = lightmap {
//             entity.insert(lightmap);
//         }
//     }
// }

#[derive(Deserialize)]
pub struct WorldSpawn {
    _classname: String,
}

struct BspMesh {
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
            .collect();

        Collider::trimesh(self.positions.clone(), indices)
    }
}

impl From<BspMesh> for Mesh {
    fn from(value: BspMesh) -> Self {
        Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::RENDER_WORLD,
        )
        .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, value.texture_uvs)
        .with_inserted_attribute(Mesh::ATTRIBUTE_UV_1, value.lightmap_uvs)
        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, value.normals)
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, value.positions)
        .with_inserted_indices(Indices::U16(value.indices))
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
            (
                face.texture().uv(position),
                lightmap_uv_rect.min + lightmap_uv_rect.size() * lightmap_uv,
                source_to_bevy(model_origin + position),
            )
        })
        .multiunzip();

    let indices: Vec<_> = face.triangulate_indices().map(|i| i as _).collect();
    let normals = vec![source_to_bevy(face.normal()); positions.len()];

    Some(BspMesh {
        positions,
        normals,
        indices,
        texture_uvs,
        lightmap_uvs,
    })
}

pub fn spawn_worldspawn(
    commands: &mut Commands,
    bsp_asset: &BspAsset,
    meshes: &mut Assets<Mesh>,
    model: vbsp::Handle<'_, vbsp::Model>,
    styles_to_image: &HashMap<LightmapStyle, (Handle<Image>, UVec2)>,
    face_to_lightmap_uv: &HashMap<u32, vbsp::Rect>,
) {
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

            let Some(mesh) = mesh_from_face(Vec3::ZERO, &face, &lightmap_uv_rect) else {
                return commands.spawn(()).id();
            };

            let collider = mesh.collider();
            let mesh_handle = meshes.add(mesh);

            let texture_name = face.texture().name().to_ascii_lowercase();

            let material = bsp_asset
                .materials
                .get(&texture_name)
                .cloned()
                .unwrap_or_else(|| {
                    warn!("No material for BSP model: {texture_name}");
                    bsp_asset.default_material.0.clone()
                });

            let mut out = commands.spawn((
                CollisionMargin(0.01),
                collider,
                RigidBody::Static,
                Mesh3d(mesh_handle),
                MeshMaterial3d(material),
            ));

            if let Some(lightmap) = lightmap_handle {
                out.insert(Lightmap {
                    image: lightmap.0.clone(),
                    ..Default::default()
                });
            }

            out.id()
        })
        .collect::<Vec<_>>();

    let Some(root) = model.root() else {
        // TODO: Handle this better
        panic!("Worldspawn model without root node!");
    };

    let root_ent = commands.spawn(()).id();

    let mut clusters = HashMap::<u32, EntityHashSet>::new();

    struct CurNode<'a> {
        entity: Entity,
        handle: vbsp::Handle<'a, vbsp::Node>,
        path_to_root: Vec<Entity>,
    }

    let mut nodes = vec![CurNode {
        entity: root_ent,
        handle: root,
        path_to_root: vec![root_ent],
    }];

    let mut parents = EntityHashMap::<Entity>::new();

    while let Some(cur) = nodes.pop() {
        let [front, back] = cur
            .handle
            .children()
            .expect("Malformed vistree")
            .map(|child| {
                let child_ent = commands.spawn((RenderLayers::none(), VisTreeElementOf { root: root_ent })).id();

                let model_face_range = model.first_face..model.first_face + model.face_count;

                match child {
                    Either::Left(node) => {
                        for (face_idx, _) in node.faces_with_id() {
                            if !model_face_range.contains( &face_idx) {
                                let missing_face = bsp_asset.bsp.face(face_idx as _);
                                error!(
                                    "Face {face_idx} found in model vistree that isn't in model range {model_face_range:?}: {missing_face:#?}"
                                );
                                continue;
                            }
                            let Some(face_entity) =
                                faces.get((face_idx - first_model_face) as usize)
                            else {
                                continue;
                            };

                            let _ = parents.try_insert(*face_entity, child_ent);
                        }

                        let mut new_path = cur.path_to_root.clone();

                        new_path.push(child_ent);

                        nodes.push(CurNode {
                            entity: child_ent,
                            handle: node,
                            path_to_root: new_path,
                        });
                    }
                    Either::Right(leaf) => {
                        for (face_idx, _) in leaf.faces_with_id() {
                            if !model_face_range.contains( &face_idx) {
                                let missing_face = bsp_asset.bsp.face(face_idx as _);
                                error!(
                                    "Face {face_idx} found in model vistree that isn't in model range {model_face_range:?}: {missing_face:#?}"
                                );
                                continue;
                            }
                            let Some(face_entity) =
                                faces.get((face_idx - first_model_face) as usize)
                            else {
                                continue;
                            };

                            parents.insert(*face_entity, child_ent);
                        }

                        if let Ok(cluster_u32) = leaf.cluster.try_into() {
                            clusters.entry(cluster_u32).or_default().extend(
                                std::iter::once(child_ent)//.chain(cur.path_to_root.iter().copied()),
                            );
                        }
                    }
                }

                child_ent
            });

        let plane = cur.handle.plane();

        let (normal, dist) = source_to_bevy(plane.normal()).normalize_and_length();
        let dist = dist * plane.dist;

        commands.entity(cur.entity).insert(VisChildren {
            front,
            back,
            midpoint: HalfSpace::new(normal.extend(dist)),
        });
    }

    for face in faces
        .iter()
        .copied()
        .filter(|face| !parents.contains_key(face))
    {
        commands
            .entity(face)
            .insert(RenderLayers::default())
            .insert(ChildOf(root_ent));
    }

    for (face, parent) in parents {
        commands
            .entity(face)
            .insert(RenderLayers::none())
            .insert(ChildOf(parent));
    }

    let mut visibility_map = EntityHashMap::<EntityHashSet>::new();

    for (cluster_idx, entities) in &clusters {
        let visible_clusters = bsp_asset.bsp.vis_data.visible_clusters(*cluster_idx);

        let visible_entities = visible_clusters.filter_map(|i| clusters.get(&i)).flatten();

        for entity in entities {
            visibility_map
                .entry(*entity)
                .or_default()
                .extend(visible_entities.clone());
        }
    }

    commands
        .entity(root_ent)
        .insert(VisClusters { visibility_map });
}

pub fn spawn_bsp_model(
    commands: &mut Commands,
    bsp_asset: &BspAsset,
    meshes: &mut Assets<Mesh>,
    model: vbsp::Handle<'_, vbsp::Model>,
    styles_to_image: &HashMap<LightmapStyle, (Handle<Image>, UVec2)>,
    face_to_lightmap_uv: &HashMap<u32, vbsp::Rect>,
    transform: Transform,
) {
    let mut meshes_to_spawn: HashMap<(String, Option<Handle<Image>>), (Mesh, Option<Lightmap>)> =
        HashMap::new();

    for (face_idx, face) in model.faces_with_id() {
        let lightmap_handle = styles_to_image.get(&LightmapStyle(face.styles[0])).cloned();
        let Some(lightmap_size) = lightmap_handle.as_ref().map(|(_, size)| size.as_vec2()) else {
            continue;
        };

        let lightmap_rect = face_to_lightmap_uv[&(face_idx as u32)];

        let min = UVec2::new(lightmap_rect.x, lightmap_rect.y).as_vec2();
        let size = UVec2::new(lightmap_rect.width, lightmap_rect.height).as_vec2();
        let lightmap_uv_rect = Rect {
            min: min / lightmap_size,
            max: (min + size) / lightmap_size,
        };

        let Some(mesh) = mesh_from_face(model.origin, &face, &lightmap_uv_rect) else {
            continue;
        };

        let mesh = mesh.into();

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
        if texture_name.contains("tools/") {
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
        let collider = Collider::trimesh_from_mesh(&mesh);
        let mesh_handle = meshes.add(mesh);

        let mut entity = commands.spawn((
            Mesh3d(mesh_handle),
            MeshMaterial3d(material.clone()),
            transform,
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
) -> impl Iterator<Item = (Mesh, Handle<StandardMaterial>)> {
    // TODO: Handle bones, since many models won't render with the correct
    // transform if they aren't handled.
    model
        .meshes()
        .zip(model.textures())
        .map(|(mdl_mesh, texture_info)| {
            let (vertices, normals, uvs): (Vec<_>, Vec<_>, Vec<_>) = mdl_mesh
                .vertices()
                .map(|v| {
                    (
                        source_to_bevy(Vec3::new(v.position.x, v.position.y, v.position.z)),
                        source_to_bevy(Vec3::new(v.normal.x, v.normal.y, v.normal.z)),
                        v.texture_coordinates,
                    )
                })
                .multiunzip();

            let indices = mdl_mesh
                .vertex_strip_indices()
                .filter_map(|idx| {
                    idx.try_into()
                        .ok()
                        .filter(|&i: &u16| i < vertices.len() as u16)
                })
                .collect::<Vec<u16>>();

            let mesh = Mesh::new(
                PrimitiveTopology::TriangleList,
                RenderAssetUsages::RENDER_WORLD,
            )
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
