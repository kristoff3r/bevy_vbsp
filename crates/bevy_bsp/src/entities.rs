pub mod info_player;

use std::collections::{HashMap, hash_map::Entry};

use avian3d::prelude::{Collider, RigidBody};
use bevy::{
    asset::RenderAssetUsages,
    camera::visibility::RenderLayers,
    mesh::{Indices, PrimitiveTopology},
    pbr::Lightmap,
    prelude::*,
};
use itertools::Itertools;
use qbsp::data::LightmapStyle;
use serde::Deserialize;

use super::{BspAsset, source_to_bevy};

#[derive(Deserialize)]
pub struct WorldSpawn {
    _classname: String,
}

fn mesh_from_face(
    model_origin: Vec3,
    face: &vbsp::Handle<'_, vbsp::Face>,
    lightmap_uv_rect: &Rect,
) -> Option<Mesh> {
    if !face.is_visible() {
        return None;
    }

    let (texture_uvs, lightmap_uvs, vertices): (Vec<Vec2>, Vec<Vec2>, Vec<Vec3>) = face
        .vertex_positions()
        .zip(face.lightmap_uvs())
        .map(|(position, lightmap_uv)| {
            (
                face.texture().uv(position),
                lightmap_uv_rect.min + lightmap_uv_rect.size() * lightmap_uv,
                Vec3::from(source_to_bevy(model_origin + position)),
            )
        })
        .multiunzip();

    let indices: Vec<_> = face.triangulate_indices().map(|i| i as _).collect();
    let normals = vec![source_to_bevy(face.normal()); vertices.len()];

    let mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::RENDER_WORLD,
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, texture_uvs)
    .with_inserted_attribute(Mesh::ATTRIBUTE_UV_1, lightmap_uvs)
    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, normals)
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, vertices)
    .with_inserted_indices(Indices::U16(indices));

    Some(mesh)
}

pub fn spawn_worldspawn(
    commands: &mut Commands,
    bsp_asset: &BspAsset,
    meshes: &mut Assets<Mesh>,
    styles_to_image: &HashMap<LightmapStyle, (Handle<Image>, UVec2)>,
    face_to_lightmap_uv: &HashMap<u32, vbsp::Rect>,
    transform: Transform,
) {
    let mut cluster_meshes: HashMap<(String, Option<Handle<Image>>), (Mesh, Option<Lightmap>)> =
        HashMap::new();

    let cluster_meshes = bsp_asset
        .bsp
        .root_node()
        .vis_clusters()
        .into_iter()
        .flat_map(|(cluster_idx, cluster)| {
            for (face_idx, face) in cluster.into_iter().flat_map(|leaf| leaf.faces_with_id()) {
                let lightmap_handle = styles_to_image.get(&LightmapStyle(face.styles[0])).cloned();
                let Some(lightmap_size) = lightmap_handle.as_ref().map(|(_, size)| size.as_vec2())
                else {
                    continue;
                };

                let lightmap_rect = face_to_lightmap_uv[&face_idx];

                let min = UVec2::new(lightmap_rect.x, lightmap_rect.y).as_vec2();
                let size = UVec2::new(lightmap_rect.width, lightmap_rect.height).as_vec2();
                let lightmap_uv_rect = Rect {
                    min: min / lightmap_size,
                    max: (min + size) / lightmap_size,
                };

                let Some(mesh) = mesh_from_face(Vec3::ZERO, &face, &lightmap_uv_rect) else {
                    continue;
                };

                match cluster_meshes.entry((
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

            cluster_meshes
                .drain()
                .map(move |((texture_name, _), (mesh, lightmap))| {
                    (cluster_idx, texture_name, mesh, lightmap)
                })
                .collect::<Vec<_>>()
        });

    for (_cluster_idx, texture_name, mesh, lightmap) in cluster_meshes {
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

        // let render_layers = cluster_idx
        //     .try_into()
        //     .map(|cluster_idx_usize| {
        //         std::iter::once(cluster_idx_usize)
        //             .chain(
        //                 bsp_asset
        //                     .bsp
        //                     .vis_data
        //                     .visible_clusters(cluster_idx)
        //                     .iter()
        //                     .enumerate()
        //                     .filter(|(_, visible)| *visible)
        //                     .map(|(i, _)| i),
        //             )
        //             // `+ 1` so we have space to add the `0` layer.
        //             .map(|idx| idx + 1)
        //             .collect::<RenderLayers>()
        //     })
        //     .unwrap_or_default();

        let mut entity = commands.spawn((
            // dbg!(render_layers),
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
    model
        .meshes()
        .zip(model.textures())
        .map(|(mdl_mesh, texture_info)| {
            let (vertices, normals, uvs): (Vec<_>, Vec<_>, Vec<_>) = mdl_mesh
                .vertices()
                .iter()
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
