pub mod info_player;

use avian3d::prelude::{Collider, Mass, RigidBody};
use bevy::{
    asset::RenderAssetUsages,
    mesh::{Indices, PrimitiveTopology},
    pbr::Lightmap,
    platform::collections::{HashMap, hash_map::Entry},
    prelude::*,
};
use image::{DynamicImage, Rgba32FImage};
use qbsp::{
    data::LightmapStyle,
    mesh::lightmap::{DefaultLightmapPacker, PerStyleLightmapData},
};
use serde::Deserialize;

use super::{BspAsset, source_to_bevy};

#[derive(Deserialize)]
pub struct WorldSpawn {
    _classname: String,
}

pub fn spawn_bsp_model(
    commands: &mut Commands,
    bsp_asset: &BspAsset,
    meshes: &mut Assets<Mesh>,
    images: &mut Assets<Image>,
    model: vbsp::Handle<'_, vbsp::Model>,
    transform: Transform,
) {
    let mut meshes_to_spawn: HashMap<(String, Option<Handle<Image>>), (Mesh, Option<Lightmap>)> =
        HashMap::new();

    let packer =
        DefaultLightmapPacker::<PerStyleLightmapData<Rgba32FImage>>::new(Default::default());
    let atlas = bsp_asset
        .bsp
        .compute_lightmap_atlas(packer)
        .expect("Could not build atlas");

    let styles_to_image = atlas
        .data
        .into_inner()
        .into_iter()
        .map(|(style, img)| {
            let dynamic: DynamicImage = img.into();

            dynamic.save(format!("lm-out-{style}.exr")).unwrap();

            let image = Image::from_dynamic(dynamic, true, RenderAssetUsages::RENDER_WORLD);

            (style, images.add(image))
        })
        .collect::<HashMap<_, _>>();

    let faces_with_idx = {
        let start = model.first_face as usize;
        let end = start + model.face_count as usize;
        let bsp = &bsp_asset.bsp;

        bsp.faces[start..end]
            .iter()
            .map(move |face| vbsp::Handle::new(bsp, face))
            .zip(start..end)
    };

    for (face, idx) in faces_with_idx {
        if !face.is_visible() {
            continue;
        }

        let lightmap_handle = styles_to_image.get(&LightmapStyle(face.styles[0])).cloned();

        let raw_vertices = face.vertex_positions().collect::<Vec<_>>();
        let vertices = raw_vertices
            .iter()
            .cloned()
            .map(source_to_bevy)
            .collect::<Vec<_>>();
        let indices = raw_vertices
            .iter()
            .enumerate()
            .map(|(index, _)| index as u16)
            .collect::<Vec<_>>();
        let normals = vertices
            .iter()
            .map(|_| source_to_bevy(face.normal()))
            .collect::<Vec<_>>();

        let texture = face.texture();
        let uvs = raw_vertices
            .iter()
            .copied()
            .map(|pos| texture.uv(pos))
            .collect::<Vec<_>>();

        let mesh = Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::RENDER_WORLD,
        )
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, vertices)
        .with_inserted_indices(Indices::U16(indices))
        .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uvs)
        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, normals);

        let mesh = if let Some(lightmap_uvs) = atlas.uvs.get(&(idx as u32)) {
            mesh.with_inserted_attribute(
                Mesh::ATTRIBUTE_UV_1,
                lightmap_uvs.into_iter().copied().collect::<Vec<_>>(),
            )
        } else {
            mesh
        };

        match meshes_to_spawn.entry((
            face.texture().name().to_ascii_lowercase(),
            lightmap_handle.clone(),
        )) {
            Entry::Occupied(mut entry) => {
                let entry = entry.get_mut();
                entry.0.merge(&mesh).unwrap();
            }
            Entry::Vacant(entry) => {
                let lightmap = lightmap_handle.map(|image| Lightmap {
                    image: image.clone(),
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
    commands: &mut Commands,
    bsp_asset: &BspAsset,
    meshes: &mut Assets<Mesh>,
    model: &vmdl::Model,
    transform: Transform,
    rigid_body: RigidBody,
) {
    for (mdl_mesh, texture_info) in model.meshes().zip(model.textures()) {
        for strip in mdl_mesh.vertex_strip_indices() {
            let mut vertices = Vec::new();
            let mut normals = Vec::new();
            let mut uvs = Vec::new();
            let mut indices = Vec::new();

            for (idx, i) in strip.enumerate() {
                let v = model.vertices()[i];
                vertices.push(source_to_bevy(Vec3 {
                    x: v.position.x,
                    y: v.position.y,
                    z: v.position.z,
                }));
                normals.push(source_to_bevy(Vec3 {
                    x: v.normal.x,
                    y: v.normal.y,
                    z: v.normal.z,
                }));
                uvs.push([v.texture_coordinates[0], v.texture_coordinates[1]]);
                indices.push(idx as u16);
            }

            let mesh = Mesh::new(
                PrimitiveTopology::TriangleList,
                RenderAssetUsages::RENDER_WORLD,
            )
            .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, vertices)
            .with_inserted_indices(Indices::U16(indices))
            .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uvs)
            .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, normals);

            let collider = Collider::trimesh_from_mesh(&mesh);
            let mesh_handle = meshes.add(mesh);

            let mut material = None;

            for search_path in &texture_info.search_paths {
                let texture_path = format!(
                    "{}{}",
                    search_path.to_ascii_lowercase(),
                    texture_info.name.to_ascii_lowercase(),
                );
                if let Some(mat) = bsp_asset.materials.get(&texture_path) {
                    material = Some(mat.clone());
                    break;
                }
            }

            let material = material.unwrap_or_else(|| {
                warn!("No material for MDL model: {:?}", texture_info);
                bsp_asset.default_material.0.clone()
            });

            let mut entity = commands.spawn((
                //
                Mesh3d(mesh_handle),
                MeshMaterial3d(material),
                transform,
            ));

            if let Some(collider) = collider {
                entity.insert((collider, rigid_body, Mass(20.0)));
            } else {
                warn!("No collider for MDL model: {}", texture_info.name);
            }
        }
    }
}
