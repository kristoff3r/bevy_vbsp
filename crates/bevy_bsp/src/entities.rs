pub mod info_player;

use std::{
    borrow::Cow,
    collections::{HashMap, hash_map::Entry},
};

use avian3d::prelude::{Collider, Mass, RigidBody};
use bevy::{
    asset::RenderAssetUsages,
    mesh::{Indices, PrimitiveTopology},
    pbr::Lightmap,
    prelude::*,
    render::render_resource::{AstcBlock, AstcChannel, Extent3d, TextureDimension, TextureFormat},
};
use image::{Rgba, Rgba32FImage};
use itertools::Itertools;
use qbsp::{
    data::LightmapStyle,
    mesh::lightmap::{DefaultLightmapPacker, LightmapAtlas, PerStyleLightmapData},
};
use serde::Deserialize;

use super::{BspAsset, source_to_bevy};

#[derive(Deserialize)]
pub struct WorldSpawn {
    _classname: String,
}

const ASTC_BLOCK_SIZE: astcenc_rs::Extents = astcenc_rs::Extents { x: 12, y: 12, z: 1 };

fn astc_convert(image: &Rgba32FImage) -> Image {
    let config = astcenc_rs::ConfigBuilder::new()
        .with_profile(astcenc_rs::Profile::HdrRgbLdrA)
        .with_block_size(ASTC_BLOCK_SIZE)
        .build()
        .unwrap();
    let mut context = astcenc_rs::Context::new(config).unwrap();

    let width;
    let height;
    let pixels = if image.width().is_multiple_of(ASTC_BLOCK_SIZE.x)
        && image.height().is_multiple_of(ASTC_BLOCK_SIZE.y)
    {
        width = image.width();
        height = image.height();
        Cow::Borrowed(&**image)
    } else {
        width = image.width().div_ceil(ASTC_BLOCK_SIZE.x) * ASTC_BLOCK_SIZE.x;
        height = image.height().div_ceil(ASTC_BLOCK_SIZE.y) * ASTC_BLOCK_SIZE.y;

        let pixels = image
            .rows()
            .flat_map(|row| {
                row.copied().chain(std::iter::repeat_n(
                    Rgba::<f32>([0.; 4]),
                    (width - image.width()) as _,
                ))
            })
            .chain(
                std::iter::repeat_n(
                    std::iter::repeat_n(Rgba::<f32>([0.; 4]), width as usize),
                    (height - image.height()) as usize,
                )
                .flatten(),
            )
            .flat_map(|pixel| pixel.0)
            .collect::<Vec<_>>();

        Cow::Owned(pixels)
    };

    let image_to_encode = astcenc_rs::Image {
        extents: astcenc_rs::Extents {
            x: width,
            y: height,
            z: 1,
        },
        data: &[&*pixels][..],
    };

    let astc_bytes = context
        .compress(&image_to_encode, astcenc_rs::Swizzle::rgba())
        .unwrap();

    Image::new(
        Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        astc_bytes,
        TextureFormat::Astc {
            block: AstcBlock::B12x12,
            channel: AstcChannel::Hdr,
        },
        RenderAssetUsages::RENDER_WORLD,
    )
}

fn mesh_from_face(
    face_idx: u32,
    face: &vbsp::Handle<'_, vbsp::FaceV2>,
    atlas_size: Vec2,
    lightmap_uv_factor: Vec2,
    lightmap_uv_offsets: &HashMap<u32, Vec2>,
) -> Option<Mesh> {
    if !face.is_visible() {
        return None;
    }

    let lightmap_offset = lightmap_uv_offsets[&(face_idx as u32)];

    let (texture_uvs, lightmap_uvs, vertices): (Vec<Vec2>, Vec<Vec2>, Vec<Vec3>) = face
        .vertex_positions()
        .map(|position| {
            let lightmap_uv = lightmap_offset + face.texture().lightmap_uv(position)
                - face.light_map_texture_min.as_vec2();

            (
                face.texture().uv(position),
                lightmap_uv_factor * lightmap_uv / atlas_size,
                Vec3::from(source_to_bevy(position)),
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

pub fn spawn_bsp_model(
    commands: &mut Commands,
    bsp_asset: &BspAsset,
    meshes: &mut Assets<Mesh>,
    images: &mut Assets<Image>,
    model: vbsp::Handle<'_, vbsp::Model>,
    transform: Transform,
) {
    const USE_ASTC: bool = false;
    const EXTRUSION: u32 = if USE_ASTC {
        if ASTC_BLOCK_SIZE.x > ASTC_BLOCK_SIZE.y {
            ASTC_BLOCK_SIZE.x
        } else {
            ASTC_BLOCK_SIZE.y
        }
    } else {
        1
    };

    let mut meshes_to_spawn: HashMap<(String, Option<Handle<Image>>), (Mesh, Option<Lightmap>)> =
        HashMap::new();

    let packer = DefaultLightmapPacker::<PerStyleLightmapData<Rgba32FImage>>::new(
        qbsp::prelude::ComputeLightmapSettings {
            extrusion: EXTRUSION,
            ..Default::default()
        },
    );
    let atlas = bsp_asset
        .bsp
        .compute_lightmap_atlas(packer)
        .expect("Could not build atlas");

    let atlas_size = atlas.data.size();

    let styles_to_image = atlas
        .data
        .into_inner()
        .into_iter()
        .map(|(style, img)| {
            let original_width = img.width();
            let original_height = img.height();

            let gpu_image = if USE_ASTC {
                astc_convert(&img)
            } else {
                Image::from_dynamic(img.into(), true, RenderAssetUsages::RENDER_WORLD)
            };

            let lightmap_uv_factor = Vec2::new(
                original_width as f32 / gpu_image.width() as f32,
                original_height as f32 / gpu_image.height() as f32,
            );
            (style, (images.add(gpu_image), lightmap_uv_factor))
        })
        .collect::<HashMap<_, _>>();

    for (face_idx, face) in model.faces_with_id() {
        let lightmap_handle = styles_to_image.get(&LightmapStyle(face.styles[0])).cloned();

        let lightmap_uv_factor = lightmap_handle
            .as_ref()
            .map(|(_, factor)| *factor)
            .unwrap_or(Vec2::splat(1.));

        let Some(mesh) = mesh_from_face(
            face_idx as _,
            &face,
            atlas_size.as_vec2(),
            lightmap_uv_factor,
            &atlas.offsets,
        ) else {
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
        // let collider = Collider::trimesh_from_mesh(&mesh);
        let mesh_handle = meshes.add(mesh);

        let mut entity = commands.spawn((
            Mesh3d(mesh_handle),
            MeshMaterial3d(material.clone()),
            transform,
        ));

        if let Some(lightmap) = lightmap {
            entity.insert(lightmap);
        }

        // if let Some(collider) = collider {
        //     entity.insert((collider, RigidBody::Static));
        // } else {
        //     warn!("No collider for texture: {}", texture_name);
        // }
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

            let texture_path = texture_info.name.to_ascii_lowercase();
            let material = bsp_asset.materials.get(&texture_path).unwrap_or_else(|| {
                warn!("No material for MDL model: {:?}", texture_info);
                &bsp_asset.default_material.0
            });

            let mut entity = commands.spawn((
                //
                Mesh3d(mesh_handle),
                MeshMaterial3d(material.clone()),
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
