pub mod crosshair_pointer;
pub mod debug;
pub mod entities;
pub mod matcher;
pub mod visdata;

use core::panic;
use std::{
    borrow::Cow,
    ffi::OsStr,
    ops::Deref,
    path::{Path, PathBuf},
    result::Result,
    str::FromStr,
    sync::Arc,
};

use anyhow::bail;
use avian3d::prelude::{Collider, RigidBody};
use bevy::{
    asset::{AssetLoader, AssetPath, LoadContext, RenderAssetUsages, io::Reader},
    core_pipeline::Skybox,
    image::{ImageAddressMode, ImageSampler, ImageSamplerDescriptor, TextureFormatPixelInfo},
    math::{Affine3A, primitives},
    mesh::PrimitiveTopology,
    pbr::Lightmap,
    platform::collections::{HashMap, hash_map::Entry},
    prelude::*,
    render::render_resource::{
        AstcBlock, AstcChannel, Extent3d, TextureDimension, TextureFormat, TextureViewDescriptor,
        TextureViewDimension,
    },
};
use entities::spawn_bsp_model;
use image::{Rgba32FImage, imageops::FilterType};
use itertools::Either;
use qbsp::{
    data::LightmapStyle,
    mesh::lightmap::{DefaultLightmapPacker, PerStyleLightmapData},
};
use serde::{Deserialize, Serialize};
use vbsp::{Angles, Bsp, GenericEntity, StaticPropLumpFlags};

use bevy_vpk::{vmt::VmtAssetLoader, vtf::VtfAssetLoader};
use tracing::instrument;

use entities::{BspEntityModelMesh, BspStaticPropMesh, spawn_mdl_model, spawn_worldspawn};

// Re-export everything while we use a lot of git dependencies
pub use bevy_vpk;
pub use qbsp;
pub use vbsp;
pub use vdf_reader;
pub use vmdl;
pub use vmt_parser;
pub use vpk;

use crate::{
    entities::FaceSpawner,
    matcher::{AnyString, Not, StringMatcher},
};

pub struct BspLoaderPlugin;

pub const SCALE: f32 = 39.37008f32.recip();

#[derive(Resource)]
pub struct MapAssets {
    pub bsp: Handle<BspAsset>,
}

pub const SOURCE_TO_BEVY: Affine3A = Affine3A {
    matrix3: Mat3A {
        x_axis: Vec3A::new(0., 0., -SCALE),
        y_axis: Vec3A::new(-SCALE, 0., 0.),
        z_axis: Vec3A::new(0., SCALE, 0.),
    },
    translation: Vec3A::ZERO,
};

pub fn parse_vec3(s: &str) -> Result<Vec3, <f32 as FromStr>::Err> {
    let mut parts = s.split(' ');
    Ok([
        parts.next().unwrap_or_default().parse()?,
        parts.next().unwrap_or_default().parse()?,
        parts.next().unwrap_or_default().parse()?,
    ]
    .into())
}

impl Plugin for BspLoaderPlugin {
    fn build(&self, app: &mut App) {
        app.init_asset::<BspAsset>()
            .init_asset_loader::<BspAssetLoader>()
            .init_asset_loader::<VtfAssetLoader>()
            .init_asset_loader::<VmtAssetLoader>()
            .with_bsp_class(
                "worldspawn",
                |In(entity): In<Entity>,
                 mut commands: Commands,
                 mut meshes: ResMut<Assets<Mesh>>,
                 entities: Query<&BspEntity>,
                 global_infos: Query<&GlobalBspInfo>,
                 bsp_assets: Res<Assets<BspAsset>>| {
                    let Ok(entity) = entities.get(entity) else {
                        return;
                    };

                    let Ok(bsp) = global_infos.get(entity.bsp) else {
                        return;
                    };

                    let Some(bsp_asset) = bsp_assets.get(&bsp.bsp) else {
                        return;
                    };

                    spawn_worldspawn::<DefaultFaceSpawner>(
                        &mut commands,
                        bsp_asset,
                        &mut meshes,
                        bsp_asset.bsp.models().next().expect("No worldspawn"),
                        &bsp.styles_to_image,
                        &bsp.atlas_rects,
                    );
                },
            )
            .with_bsp_property(
                Not("worldspawn"),
                "model",
                |In(entity): In<Entity>,
                 mut commands: Commands,
                 mut meshes: ResMut<Assets<Mesh>>,
                 mut global_infos: Query<&mut GlobalBspInfo>,
                 entities: Query<&BspEntity>,
                 bsp_assets: Res<Assets<BspAsset>>| {
                    let Ok(entity) = entities.get(entity) else {
                        return;
                    };

                    let Ok(mut bsp) = global_infos.get_mut(entity.bsp) else {
                        return;
                    };

                    let Some(bsp_asset) = bsp_assets.get(&bsp.bsp) else {
                        return;
                    };

                    let Some(model) = entity.data.get("model") else {
                        return;
                    };

                    let origin = entity
                        .data
                        .get("origin")
                        .and_then(|e| e.as_value())
                        .and_then(|s| parse_vec3(s).ok())
                        .unwrap_or_default();

                    let angles: Angles = entity
                        .data
                        .get("angles")
                        .and_then(|e| e.as_value())
                        .and_then(|s| s.parse().ok())
                        .unwrap_or_default();

                    let angles = angles.as_quaternion();
                    let quat = Quat::from_xyzw(angles.x, angles.y, angles.z, angles.w);
                    let transform = Transform::from_matrix(SOURCE_TO_BEVY.into())
                        * Transform::from_translation(origin).with_rotation(quat);

                    if let Some(model) = model.as_value() {
                        // TODO: This redoes work if the same BSP model is used multiple times - does this happen in practice?
                        if model.starts_with("*") {
                            let idx: usize = model.deref().split_at(1).1.parse().unwrap();
                            let model_handle = bsp_asset.bsp.models().nth(idx).unwrap();
                            spawn_bsp_model::<DefaultFaceSpawner>(
                                &mut commands,
                                bsp_asset,
                                &mut meshes,
                                model_handle,
                                &bsp.styles_to_image,
                                &bsp.atlas_rects,
                                transform,
                                &entity.class,
                                idx,
                            );
                        } else {
                            let occupied_ref;
                            let processed_mdl =
                                match bsp.processed_models.entry(model.deref().to_owned()) {
                                    Entry::Occupied(occupied_entry) => {
                                        occupied_ref = occupied_entry;
                                        occupied_ref.get()
                                    }
                                    Entry::Vacant(vacant_entry) => {
                                        let Some(model) =
                                            bsp_asset.models.get(&vacant_entry.key()[..])
                                        else {
                                            return;
                                        };

                                        vacant_entry.insert(ProcessedMdl::new(
                                            spawn_mdl_model(bsp_asset, model),
                                            &mut meshes,
                                        ))
                                    }
                                };

                            if let Some(collider) = processed_mdl.collider.clone() {
                                commands.spawn((collider, RigidBody::Dynamic, transform));
                            }

                            for VMdlComponent { mesh, material } in &processed_mdl.components {
                                commands.spawn((
                                    BspEntityModelMesh {
                                        model_path: model.to_string(),
                                        classname: entity.class.clone(),
                                    },
                                    Mesh3d(mesh.clone()),
                                    MeshMaterial3d(material.clone()),
                                    transform,
                                    DefaultFaceSpawner::orphaned_face_bundle(),
                                ));
                            }
                        }
                    }
                },
            );
    }
}

const ASTC_BLOCK_SIZES: &[(AstcBlock, astcenc_rs::Extents)] = &[
    (AstcBlock::B4x4, astcenc_rs::Extents { x: 4, y: 4, z: 1 }),
    (AstcBlock::B5x4, astcenc_rs::Extents { x: 5, y: 4, z: 1 }),
    (AstcBlock::B5x5, astcenc_rs::Extents { x: 5, y: 5, z: 1 }),
    (AstcBlock::B6x5, astcenc_rs::Extents { x: 6, y: 5, z: 1 }),
    (AstcBlock::B6x6, astcenc_rs::Extents { x: 6, y: 6, z: 1 }),
    (AstcBlock::B8x5, astcenc_rs::Extents { x: 8, y: 5, z: 1 }),
    (AstcBlock::B8x6, astcenc_rs::Extents { x: 8, y: 6, z: 1 }),
    (AstcBlock::B8x8, astcenc_rs::Extents { x: 8, y: 8, z: 1 }),
    (AstcBlock::B10x5, astcenc_rs::Extents { x: 10, y: 5, z: 1 }),
    (AstcBlock::B10x6, astcenc_rs::Extents { x: 10, y: 6, z: 1 }),
    (AstcBlock::B10x8, astcenc_rs::Extents { x: 10, y: 8, z: 1 }),
    (
        AstcBlock::B10x10,
        astcenc_rs::Extents { x: 10, y: 10, z: 1 },
    ),
    (
        AstcBlock::B12x10,
        astcenc_rs::Extents { x: 12, y: 10, z: 1 },
    ),
    (
        AstcBlock::B12x12,
        astcenc_rs::Extents { x: 12, y: 12, z: 1 },
    ),
];

const fn extents(block_size: AstcBlock) -> Option<astcenc_rs::Extents> {
    let mut i = 0;

    while i < ASTC_BLOCK_SIZES.len() {
        let (check_block_size, extents) = ASTC_BLOCK_SIZES[i];
        if check_block_size as usize == block_size as usize {
            return Some(extents);
        }

        i += 1;
    }

    None
}

fn astc_convert(image: &Rgba32FImage, block_size: AstcBlock) -> Image {
    let extents = extents(block_size).unwrap();

    let config = astcenc_rs::ConfigBuilder::new()
        .with_profile(astcenc_rs::Profile::HdrRgbLdrA)
        .with_preset(astcenc_rs::PRESET_THOROUGH)
        .with_block_size(extents)
        .build()
        .unwrap();
    let mut context = astcenc_rs::Context::new(config).unwrap();

    let width = image.width().next_multiple_of(extents.x);
    let height = image.height().next_multiple_of(extents.y);

    let pixels = if width == image.width() && height == image.height() {
        Cow::Borrowed(&**image)
    } else {
        let pixels = image
            .rows()
            .enumerate()
            .flat_map(|(row_idx, row)| {
                let last = *image.get_pixel(image.width() - 1, row_idx as _);
                row.copied()
                    .chain(std::iter::repeat_n(last, (width - image.width()) as usize))
            })
            .chain({
                let last = *image.get_pixel(image.width() - 1, image.height() - 1);
                std::iter::repeat_n(
                    image
                        .rows()
                        .next_back()
                        .unwrap()
                        .copied()
                        .chain(std::iter::repeat_n(last, (width - image.width()) as usize)),
                    (height - image.height()) as usize,
                )
                .flatten()
            })
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
        .compress(&image_to_encode, astcenc_rs::Swizzle::rgb1())
        .unwrap();

    #[cfg(feature = "humansize")]
    {
        info!(
            "Input lightmap size: {}",
            humansize::format_size(pixels.len(), humansize::DECIMAL),
        );
        info!(
            "ASTC lightmap size: {}",
            humansize::format_size(astc_bytes.len(), humansize::DECIMAL),
        );
    }

    #[cfg(not(feature = "humansize"))]
    {
        info!("Input lightmap size: {}b", pixels.len());
        info!("ASTC lightmap size: {}b", astc_bytes.len(),);
    }

    Image::new(
        Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        astc_bytes,
        TextureFormat::Astc {
            block: block_size,
            channel: AstcChannel::Hdr,
        },
        RenderAssetUsages::RENDER_WORLD,
    )
}

#[derive(Default, Copy, Clone, PartialEq, Eq, Debug)]
pub struct LightmapSettings {
    pub astc_block_size: Option<AstcBlock>,
}

// TODO: This should be a relationship, but `vbsp::GenericEntity` doesn't implement `Default` right now
#[derive(Component)]
pub struct BspEntity {
    pub entity: vbsp::GenericEntity,
    pub bsp: Entity,
}

impl Deref for BspEntity {
    type Target = vbsp::GenericEntity;

    fn deref(&self) -> &Self::Target {
        &self.entity
    }
}

// TODO: Maybe this should be something custom?
pub type NewBspEntity = In<Entity>;

pub trait BspEntityWorldExt {
    fn with_bsp_property<C, P, M, T>(
        &mut self,
        classname: C,
        property_name: P,
        handler: T,
    ) -> &mut Self
    where
        C: StringMatcher + Send + Sync + 'static,
        P: StringMatcher + Send + Sync + 'static,
        T: IntoSystem<NewBspEntity, (), M> + Send + Sync + 'static;

    fn with_bsp_class<C, M, T>(&mut self, classname: C, handler: T) -> &mut Self
    where
        C: StringMatcher + Send + Sync + 'static,
        T: IntoSystem<NewBspEntity, (), M> + Send + Sync + 'static,
    {
        self.with_bsp_property(classname, AnyString, handler)
    }
}

impl BspEntityWorldExt for World {
    fn with_bsp_property<C, P, M, T>(
        &mut self,
        classname: C,
        property_name: P,
        handler: T,
    ) -> &mut Self
    where
        C: StringMatcher + Send + Sync + 'static,
        P: StringMatcher + Send + Sync + 'static,
        T: IntoSystem<NewBspEntity, (), M> + Send + Sync + 'static,
    {
        let system_id = self.register_system(handler);
        // TODO: Might benefit from resolving https://github.com/bevyengine/bevy/issues/21658
        self.add_observer(
            move |event: On<Insert, BspEntity>,
                  bsp_entities: Query<&BspEntity>,
                  mut commands: Commands| {
                let entity = event.entity;
                if let Ok(bsp_ent) = bsp_entities.get(entity)
                    && classname.is_match(&bsp_ent.class)
                    && bsp_ent.data.keys().any(|key| property_name.is_match(key))
                {
                    commands.run_system_with(system_id, entity);
                }
            },
        )
        .into_world_mut()
    }
}

impl BspEntityWorldExt for App {
    fn with_bsp_property<C, P, M, T>(
        &mut self,
        classname: C,
        property_name: P,
        handler: T,
    ) -> &mut Self
    where
        C: StringMatcher + Send + Sync + 'static,
        P: StringMatcher + Send + Sync + 'static,
        T: IntoSystem<NewBspEntity, (), M> + Send + Sync + 'static,
    {
        self.world_mut()
            .with_bsp_property(classname, property_name, handler);
        self
    }
}

#[derive(Component)]
pub struct GlobalBspInfo {
    // TODO: This is probably better done with a dense `Vec` where unset styles use `Handle::default`
    pub styles_to_image: HashMap<LightmapStyle, (Handle<Image>, UVec2)>,
    pub processed_models: HashMap<String, ProcessedMdl>,
    pub bsp: Handle<BspAsset>,
    pub atlas_rects: HashMap<u32, vbsp::Rect>,
}

#[derive(Reflect)]
pub struct VMdlComponent {
    pub mesh: Handle<Mesh>,
    pub material: Handle<StandardMaterial>,
}

pub struct ProcessedMdl {
    pub components: Vec<VMdlComponent>,
    pub collider: Option<Collider>,
}

impl ProcessedMdl {
    pub fn new<I>(components: I, meshes: &mut Assets<Mesh>) -> Self
    where
        I: IntoIterator<Item = (Mesh, Handle<StandardMaterial>)>,
    {
        let mut combined_mesh = Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::MAIN_WORLD,
        );

        let components = components
            .into_iter()
            .map(|(mesh, material)| {
                combined_mesh.merge(&mesh).unwrap();
                VMdlComponent {
                    mesh: meshes.add(mesh),
                    material,
                }
            })
            .collect();

        Self {
            components,
            collider: Collider::convex_decomposition_from_mesh(&combined_mesh),
        }
    }
}

#[cfg(not(feature = "visdata"))]
type DefaultFaceSpawner = crate::entities::GlobalFaceSpawner;

#[cfg(feature = "visdata")]
type DefaultFaceSpawner = crate::entities::VisclusterFaceSpawner;

pub fn spawn_map_entities(
    In(lightmap_settings): In<LightmapSettings>,
    mut commands: Commands,
    map_assets: Res<MapAssets>,
    mut meshes: ResMut<Assets<Mesh>>,
    bsp_asset_data: Res<Assets<BspAsset>>,
    mut images: ResMut<Assets<Image>>,
    camera: Option<Single<Entity, With<Camera>>>,
) {
    let extrusion = if let Some(block_size) = lightmap_settings.astc_block_size
        && let Some(extents) = extents(block_size)
    {
        extents.x.max(extents.y) / 2
    } else {
        2
    };

    let bsp_asset = bsp_asset_data.get(&map_assets.bsp).cloned().unwrap();
    let bsp = &bsp_asset.bsp;

    let packer = DefaultLightmapPacker::<PerStyleLightmapData<Rgba32FImage>>::new(
        qbsp::prelude::ComputeLightmapSettings {
            extrusion,
            ..Default::default()
        },
    );

    let atlas = bsp
        .compute_lightmap_atlas_rgb32f(packer)
        .expect("Could not build atlas");

    let atlas_rects = atlas.rects.into_iter().collect();
    let styles_to_image = atlas
        .data
        .into_inner()
        .into_iter()
        .map(|(style, img)| {
            let gpu_image = if let Some(block_size) = lightmap_settings.astc_block_size {
                astc_convert(&img, block_size)
            } else {
                Image::from_dynamic(img.into(), true, RenderAssetUsages::RENDER_WORLD)
            };

            let size = gpu_image.size();

            (style, (images.add(gpu_image), size))
        })
        .collect::<HashMap<_, _>>();

    info!("Loaded BSP models: {}", bsp.models().count());

    let mut processed_models: HashMap<String, ProcessedMdl> = Default::default();

    let spawn_point_mesh = meshes.add(primitives::Cuboid {
        half_size: Vec3::splat(0.5 * SCALE.recip()),
    });

    for transform in bsp_asset
        .t_spawn_points
        .iter()
        .chain(bsp_asset.ct_spawn_points.iter())
    {
        commands.spawn((
            Name::new("Spawn Point"),
            *transform,
            bsp_asset.default_material.clone(),
            Mesh3d(spawn_point_mesh.clone()),
        ));
    }

    for (i, static_prop) in bsp.static_props().enumerate() {
        if static_prop.flags.contains(StaticPropLumpFlags::NO_DRAW) {
            continue;
        }

        let name = bsp.static_props.dict.name[static_prop.prop_type as usize]
            .as_str()
            .to_ascii_lowercase();

        let quat = static_prop.angles.as_quaternion();
        let quat = Quat::from_xyzw(quat.x, quat.y, quat.z, quat.w);
        let transform = Transform::from_matrix(SOURCE_TO_BEVY.into())
            * Transform::from_translation(Vec3::new(
                static_prop.origin.x,
                static_prop.origin.y,
                static_prop.origin.z,
            ))
            .with_rotation(quat);

        let vhv;
        let mut vertex_lighting = None;
        let mut has_lighting = false;

        let vertex_light_disabled = static_prop
            .flags
            .contains(StaticPropLumpFlags::NO_PER_VERTEX_LIGHTING);

        if !vertex_light_disabled
            && let Some(bytes) = bsp
                .pack
                .get(&format!("sp_hdr_{i}.vhv"))
                .unwrap()
                .or_else(|| bsp.pack.get(&format!("sp_{i}.vhv")).unwrap())
        {
            vhv = vmdl::vhv::Vhv::read(&bytes).unwrap();

            vertex_lighting = Some(
                &vhv.meshes
                    .iter()
                    .min_by_key(|mesh| mesh.header.lod)
                    .unwrap()
                    .vertices,
            );

            has_lighting = true;
        }

        let ppl;
        let mut lightmap = None;

        let lightmap_disabled = static_prop
            .flags
            .contains(StaticPropLumpFlags::NO_PER_TEXEL_LIGHTING);

        if !lightmap_disabled
            && let Some(bytes) = bsp.pack.get(&format!("texelslighting_{i}.ppl")).unwrap()
        {
            // TODO: Not sure why the texel color seems to be at a different scale to both
            // regular lightmaps and vertex colors, but we just scale it for now.
            const TEXEL_COLOR_SCALE: f32 = 128.;

            ppl = vtf::ppl::Ppl::read(&bytes).unwrap();

            let image = &ppl
                .meshes
                .iter()
                .min_by_key(|mesh| mesh.header.lod)
                .unwrap()
                .data;
            let image = Rgba32FImage::from_vec(
                image.width(),
                image.height(),
                image
                    .as_raw()
                    .chunks_exact(3)
                    .flat_map(|rgb| {
                        let rgb: &[u8; 3] = rgb.try_into().unwrap();
                        let [r, g, b] =
                            rgb.map(|i| (i as f32 / u8::MAX as f32) * TEXEL_COLOR_SCALE);

                        [r, g, b, 1.]
                    })
                    .collect(),
            )
            .unwrap();

            let gpu_image = if let Some(block_size) = lightmap_settings.astc_block_size {
                astc_convert(&image, block_size)
            } else {
                Image::from_dynamic(image.into(), true, RenderAssetUsages::RENDER_WORLD)
            };

            let handle = images.add(gpu_image);

            vertex_lighting = None;
            lightmap = Some(handle);

            has_lighting = true;
        }

        let occupied_ref;
        let processed_mdl = match processed_models.entry(name.as_str().to_owned()) {
            Entry::Occupied(occupied_entry) => {
                occupied_ref = occupied_entry;
                occupied_ref.get()
            }
            Entry::Vacant(vacant_entry) => {
                let Some(model) = bsp_asset.models.get(&vacant_entry.key()[..]) else {
                    continue;
                };

                vacant_entry.insert(ProcessedMdl::new(
                    spawn_mdl_model(&bsp_asset, model),
                    &mut meshes,
                ))
            }
        };

        let bundles = if has_lighting {
            // TODO: Not sure why the vertex color seems to be at a different scale to the
            // lightmaps, but we just scale it for now.
            const VERTEX_COLOR_SCALE: f32 = 64.;

            let meshes =
                processed_mdl
                    .components
                    .iter()
                    .filter_map(|VMdlComponent { mesh, material }| {
                        let mut mesh = meshes.get(mesh)?.clone();
                        if let Some(vertex_lighting) = vertex_lighting {
                            let colors = vertex_lighting
                                .iter()
                                .map(|color| {
                                    let [r, g, b] =
                                        color.to_rgb32f().map(|v| v / VERTEX_COLOR_SCALE);
                                    [r, g, b, 1.]
                                })
                                .chain(std::iter::repeat([1., 1., 1., 1.]))
                                .take(mesh.count_vertices())
                                .collect::<Vec<_>>();

                            mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, colors);
                        }

                        let lightmap_component = lightmap.as_ref().map(|lightmap| {
                            mesh.insert_attribute(
                                Mesh::ATTRIBUTE_UV_1,
                                mesh.attribute(Mesh::ATTRIBUTE_UV_0).unwrap().to_owned(),
                            );

                            Lightmap {
                                image: lightmap.clone(),
                                ..Default::default()
                            }
                        });

                        Some((meshes.add(mesh), material.clone(), lightmap_component))
                    });

            Either::Left(meshes)
        } else {
            Either::Right(
                processed_mdl
                    .components
                    .iter()
                    .map(|VMdlComponent { mesh, material }| (mesh.clone(), material.clone(), None)),
            )
        };

        if let Some(collider) = processed_mdl.collider.clone() {
            commands.spawn((collider, RigidBody::Static, transform));
        }

        for (mesh, material, lightmap) in bundles {
            let mut new_entity = commands.spawn((
                BspStaticPropMesh {
                    model_path: name.clone(),
                    prop_index: i,
                },
                DefaultFaceSpawner::orphaned_face_bundle(),
                Mesh3d(mesh),
                MeshMaterial3d(material),
                transform,
            ));

            if let Some(lightmap) = lightmap {
                new_entity.insert(lightmap);
            }
        }
    }

    const EXPECTED_SKYBOX_IMAGE_COUNT: u32 = 6;

    if bsp_asset.skybox_images.len() == EXPECTED_SKYBOX_IMAGE_COUNT as usize {
        let (size, format) = {
            bsp_asset
                .skybox_images
                .iter()
                .map(|img_path| {
                    let image = images.get(img_path).unwrap();

                    (image.size(), image.texture_descriptor.format)
                })
                .reduce(|a, b| {
                    assert_eq!(a.1, b.1, "Mismatched texture formats in skybox");

                    (
                        UVec2 {
                            x: a.0.x.max(b.0.x),
                            y: a.0.y.max(b.0.y),
                        },
                        a.1,
                    )
                })
                .unwrap()
        };
        let pixel_size = format.pixel_size().unwrap() as u32;
        let mut result = Image::new(
            Extent3d {
                width: size.x.max(1),
                height: size.y.max(1),
                depth_or_array_layers: EXPECTED_SKYBOX_IMAGE_COUNT,
            },
            TextureDimension::D2,
            vec![0xff; (size.x * size.y * pixel_size * EXPECTED_SKYBOX_IMAGE_COUNT) as usize],
            format,
            RenderAssetUsages::RENDER_WORLD,
        );
        for (i, handle) in bsp_asset.skybox_images.iter().enumerate() {
            let image = images.get(handle).unwrap();
            let image_owned;
            let image = if image.size() == size {
                image
            } else {
                let resized = image.clone().try_into_dynamic().unwrap().resize_to_fill(
                    size.x,
                    size.y,
                    FilterType::CatmullRom,
                );
                image_owned = Image::from_dynamic(resized, true, RenderAssetUsages::RENDER_WORLD);
                &image_owned
            };
            if let Some(slice) = result.data.as_mut() {
                let bytes = (size.x * size.y * pixel_size) as usize;
                slice[bytes * i..bytes * (i + 1)].copy_from_slice(image.data.as_ref().unwrap());
            }
        }
        result.texture_view_descriptor = Some(TextureViewDescriptor {
            dimension: Some(TextureViewDimension::Cube),
            ..default()
        });

        let image = images.add(result);

        if let Some(camera) = camera {
            commands.entity(*camera).insert((
                //
                Skybox {
                    image,
                    brightness: 1000.0,
                    ..default()
                },
            ));
        }
    }

    let world_root = commands
        .spawn(GlobalBspInfo {
            styles_to_image,
            processed_models,
            bsp: map_assets.bsp.clone(),
            atlas_rects,
        })
        .id();

    commands.spawn_batch(
        bsp.entities
            .iter()
            .map(|raw_entity| raw_entity.parse().unwrap())
            .map(move |entity| BspEntity {
                entity,
                bsp: world_root,
            })
            .collect::<Vec<_>>(),
    );
}

#[derive(Default, TypePath)]
pub struct BspAssetLoader;

/// Debug info from a VTF texture header, keyed by texture name in [`BspAsset`].
#[derive(Debug, Clone)]
pub struct VtfInfo {
    pub width: u16,
    pub height: u16,
    pub decoded_width: u32,
    pub decoded_height: u32,
    pub flags: u32,
    pub format: String,
}

#[derive(Asset, TypePath, Clone)]
pub struct BspAsset {
    pub bsp: Arc<vbsp::Bsp>,
    pub materials: Arc<HashMap<String, Handle<StandardMaterial>>>,
    /// Parsed VMT material data, keyed by texture name (for debugging).
    pub vmt_materials: Arc<HashMap<String, vmt_parser::material::Material>>,
    /// VTF header info, keyed by texture name (for debugging).
    pub vtf_info: Arc<HashMap<String, VtfInfo>>,
    pub models: Arc<HashMap<String, vmdl::Model>>,
    pub default_material: MeshMaterial3d<StandardMaterial>,
    pub cubemap: Handle<Image>,
    pub skybox_images: Vec<Handle<Image>>,
    pub t_spawn_points: Vec<Transform>,
    pub ct_spawn_points: Vec<Transform>,
}

#[derive(Clone, Default, Deserialize, Serialize)]
pub struct BspSettings;

impl AssetLoader for BspAssetLoader {
    type Asset = BspAsset;
    type Settings = BspSettings;
    type Error = anyhow::Error;

    async fn load(
        &self,
        reader: &mut dyn Reader,
        _settings: &Self::Settings,
        load_context: &mut LoadContext<'_>,
    ) -> Result<Self::Asset, Self::Error> {
        info!("Loading bsp");
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).await?;
        let bsp = vbsp::Bsp::read(&bytes)?;

        let mut materials = HashMap::new();
        let mut vmt_materials: HashMap<String, vmt_parser::material::Material> = HashMap::new();
        let mut vtf_info: HashMap<String, VtfInfo> = HashMap::new();

        let default_texture: Handle<Image> = load_context.load("images/UVCheckerMap01-512.png");
        let cubemap: Handle<Image> = load_context.load("images/labeled_skybox.png");
        let default_material = StandardMaterial {
            base_color_texture: Some(default_texture.clone()),
            perceptual_roughness: 0.8,
            reflectance: 0.2,
            metallic: 0.0,
            ..default()
        };

        let load_material = async |load_context: &mut LoadContext<'_>,
                                   name: &str|
              -> Result<
            (
                StandardMaterial,
                Option<vmt_parser::material::Material>,
                Option<VtfInfo>,
            ),
            anyhow::Error,
        > {
            let vmt_path = material_path(name);
            let (material, parsed_vmt, base_vtf_info) = if let Some(vmt_path) = vmt_path {
                let vmt_data = read_vpk_file(&bsp, load_context, &vmt_path).await?;
                let vmt = String::from_utf8(vmt_data).expect("bad vmt utf8");
                let Ok(mut vmt) = vmt_parser::from_str(&vmt) else {
                    bail!("bad vmt: {}", vmt_path);
                };

                if let vmt_parser::material::Material::Patch(mat) = vmt {
                    let include_path = mat.include.to_lowercase();
                    let base =
                        String::from_utf8(read_vpk_file(&bsp, load_context, &include_path).await?)
                            .expect("bad vmt utf8")
                            .to_ascii_lowercase();

                    vmt = mat.apply(&base).expect("bad vmt patch");
                }

                let (texture, base_vtf_info) = if let Some(name) = vmt.base_texture() {
                    match load_texture(&bsp, load_context, name).await {
                        Ok((texture, vtf)) => (Some(texture), Some(vtf)),
                        Err(_) => {
                            warn!("Using default texture for missing texture: {}", name);
                            println!("{}", std::backtrace::Backtrace::capture());
                            (Some(default_texture.clone()), None)
                        }
                    }
                } else {
                    (Some(default_texture.clone()), None)
                };

                let bump_map = if let Some(name) = vmt.bump_map() {
                    load_texture(&bsp, load_context, name)
                        .await
                        .ok()
                        .map(|(handle, _)| handle)
                } else {
                    None
                };

                let (base_color, unlit) = match &vmt {
                    vmt_parser::material::Material::UnlitGeneric(mat) => (
                        Color::srgba(mat.color.0[0], mat.color.0[1], mat.color.0[2], mat.alpha),
                        true,
                    ),
                    _ => (Color::WHITE, false),
                };

                let material = StandardMaterial {
                    base_color,
                    base_color_texture: texture,
                    normal_map_texture: bump_map,
                    perceptual_roughness: 0.8,
                    reflectance: 0.2,
                    metallic: 0.0,
                    unlit,
                    alpha_mode: if vmt.translucent() {
                        AlphaMode::Blend
                    } else if let Some(test) = vmt.alpha_test() {
                        AlphaMode::Mask(test)
                    } else {
                        AlphaMode::Opaque
                    },
                    ..default()
                };

                (material, Some(vmt), base_vtf_info)
            } else {
                let texture_name = texture_path(name);
                let (texture, vtf_info) = if let Some(texture_name) = texture_name
                    && let Ok((texture, vtf)) = load_texture(&bsp, load_context, &texture_name).await
                {
                    (Some(texture), Some(vtf))
                } else {
                    warn!("Using default texture for missing texture: {}", name);
                    println!("{}", std::backtrace::Backtrace::capture());
                    (Some(default_texture.clone()), None)
                };

                let material = StandardMaterial {
                    base_color_texture: texture,
                    perceptual_roughness: 0.8,
                    reflectance: 0.2,
                    metallic: 0.0,
                    ..default()
                };

                (material, None, vtf_info)
            };

            Ok((material, parsed_vmt, base_vtf_info))
        };

        let default_material = load_context
            .add_labeled_asset("default".to_owned(), default_material)
            .into();

        for texture in bsp.textures() {
            let name = texture.name().to_ascii_lowercase();
            if materials.contains_key(&name) {
                continue;
            }

            let Ok((material, parsed_vmt, vtf)) = load_material(load_context, &name).await else {
                warn!("Could not find material {name}");
                continue;
            };

            let material_load_context = load_context.begin_labeled_asset();
            let asset = material_load_context.finish(material);

            let mat_handle =
                load_context.add_loaded_labeled_asset::<StandardMaterial>(name.to_string(), asset);

            materials.insert(name.to_owned(), mat_handle.clone());
            if let Some(vmt) = parsed_vmt {
                vmt_materials.insert(name.to_owned(), vmt);
            }
            if let Some(vtf) = vtf {
                vtf_info.insert(name.to_owned(), vtf);
            }
        }

        let load_model = async |load_context: &mut LoadContext<'_>, path: &str| {
            let data = read_vpk_file(&bsp, load_context, path).await?;
            let mdl = vmdl::Mdl::read(&data).unwrap_or_else(|_| panic!("invalid mdl {path}"));

            let vvd_path = PathBuf::from(path).with_extension("vvd");
            let data = read_vpk_file(&bsp, load_context, vvd_path.to_str().unwrap()).await?;
            let vvd = vmdl::Vvd::read(&data)
                .unwrap_or_else(|_| panic!("invalid vvd {}", vvd_path.display()));

            let vtx_path = PathBuf::from(path).with_extension("dx90.vtx");
            let data = read_vpk_file(&bsp, load_context, vtx_path.to_str().unwrap()).await?;
            let vtx = vmdl::Vtx::read(&data)
                .unwrap_or_else(|_| panic!("invalid vtx {}", vtx_path.display()));

            Ok::<_, anyhow::Error>(vmdl::Model::from_parts(mdl, vtx, vvd))
        };

        let mut load_model_textures =
            async |load_context: &mut LoadContext<'_>, model: &vmdl::Model| {
                'outer: for texture in model.textures() {
                    let name = texture.name.to_ascii_lowercase();

                    if materials.contains_key(&name) {
                        continue;
                    }

                    for search_path in &texture.search_paths {
                        let path = format!("{}{}", search_path.to_ascii_lowercase(), name);
                        let mut material_load_context = load_context.begin_labeled_asset();
                        let asset = match load_material(&mut material_load_context, &path).await {
                            Ok((material, _, _)) => material_load_context.finish(material),
                            Err(e) => {
                                warn!("Could not load model as VMT: {e}");
                                let texture =
                                    match load_texture(&bsp, &mut material_load_context, &path)
                                        .await
                                    {
                                        Ok((texture, _)) => texture,
                                        Err(e) => {
                                            warn!("Could not load model as VMT: {e}");
                                            continue;
                                        }
                                    };
                                material_load_context.finish(StandardMaterial::from(texture))
                            }
                        };

                        let mat_handle = load_context
                            .add_loaded_labeled_asset::<StandardMaterial>(name.clone(), asset);

                        materials.insert(name, mat_handle.clone());

                        continue 'outer;
                    }

                    warn!("No material found for model texture: {}", texture.name);
                }
            };

        let mut models = HashMap::new();
        let mut t_spawn_points = Vec::new();
        let mut ct_spawn_points = Vec::new();
        for entity in &bsp.entities {
            let entity: GenericEntity = entity.parse().unwrap();
            if let Some(model) = entity.data.get("model")
                && let Some(model_key) = model.as_value()
            {
                let model_key = model_key.deref();
                if !model_key.starts_with("*") && !model_key.ends_with("vmt") {
                    if models.contains_key(model_key) {
                        continue;
                    }
                    match load_model(load_context, model_key).await {
                        Ok(model_data) => {
                            load_model_textures(load_context, &model_data).await;

                            models.insert(model_key.to_owned(), model_data);
                        }
                        Err(e) => {
                            warn!("Could not spawn model: {e}");
                        }
                    }
                }
            }
            if entity.class.starts_with("info_player") {
                let origin = entity
                    .data
                    .get("origin")
                    .and_then(|e| e.as_value())
                    .and_then(|s| {
                        let mut parts = s.split(' ');
                        Some(
                            [
                                parts.next()?.parse().ok()?,
                                parts.next()?.parse().ok()?,
                                parts.next()?.parse().ok()?,
                            ]
                            .into(),
                        )
                    })
                    .unwrap_or_default();

                let angles: Angles = entity
                    .data
                    .get("angles")
                    .and_then(|e| e.as_value())
                    .and_then(|s| s.parse().ok())
                    .unwrap_or_default();

                let quat = angles.as_quaternion();
                let quat = Quat::from_xyzw(quat.x, quat.y, quat.z, quat.w);
                let transform = Transform::from_matrix(SOURCE_TO_BEVY.into())
                    * Transform::from_translation(origin).with_rotation(quat);
                match entity.class.as_str() {
                    "info_player_terrorist" => {
                        t_spawn_points.push(transform);
                    }
                    "info_player_counterterrorist" => {
                        ct_spawn_points.push(transform);
                    }
                    // `info_player_logo` is used in `test_hardware` in CS:S
                    "info_player_start" | "info_player_teamspawn" | "info_player_logo" => {
                        t_spawn_points.push(transform)
                    }
                    _ => {
                        warn!("unknown class: {}", entity.class);
                    }
                }
            }
        }

        // TODO: Handle the leaf cluster.
        for model in &bsp.static_props.dict.name {
            let model_key = model.as_str().to_ascii_lowercase();
            if models.contains_key(&model_key) {
                continue;
            }
            let model_data = match load_model(load_context, &model_key).await {
                Ok(model_data) => model_data,
                Err(e) => {
                    warn!("model={model_key:?} not found in vpk or bsp pakfile: {e}");
                    continue;
                }
            };

            load_model_textures(load_context, &model_data).await;

            models.insert(model_key.to_owned(), model_data);
        }

        let worldspawn: GenericEntity = bsp
            .entities
            .iter()
            .find(|ent| {
                ent.properties()
                    .find_map(|(k, v)| (k == "classname").then_some(v))
                    == Some("worldspawn")
            })
            .unwrap()
            .parse()
            .unwrap();

        let skybox = worldspawn
            .data
            .get("skyname")
            .and_then(|e| e.as_value())
            .unwrap()
            .to_ascii_lowercase();

        let mut skybox_images = Vec::new();

        const SKYBOX_SIDES: &[&[&str]] = &[
            &["rt", "side"],
            &["lf", "side"],
            &["up"],
            &["dn"],
            &["ft", "side"],
            &["bk", "side"],
        ];

        'build_sides: for dir_options in SKYBOX_SIDES {
            for option in dir_options.iter() {
                let path = format!("skybox/{skybox}{option}");
                match load_texture(&bsp, load_context, &path).await {
                    Ok((image, _)) => {
                        skybox_images.push(image);
                        continue 'build_sides;
                    }
                    Err(e) => {
                        debug!("Missing skybox image {path}: {e}");
                    }
                }
            }

            warn!("Could not find side {dir_options:?} for skybox {skybox}");
        }

        Ok(BspAsset {
            bsp: Arc::new(bsp),
            materials: Arc::new(materials),
            vmt_materials: Arc::new(vmt_materials),
            vtf_info: Arc::new(vtf_info),
            models: Arc::new(models),
            default_material,
            skybox_images,
            cubemap,
            t_spawn_points,
            ct_spawn_points,
        })
    }

    fn extensions(&self) -> &[&str] {
        &["bsp"]
    }
}

fn material_path<P: AsRef<str> + ?Sized>(name: &P) -> Option<String> {
    let name = name.as_ref();
    match Path::new(name).extension() {
        // We need to normalize double-slashes.
        // TODO: We should just use paths, but we need to handle Windows vs Unix path separators.
        None => Some(format!("materials/{}.vmt", name).replace("//", "/")),
        Some(ext) if ext == OsStr::new("vmt") => {
            Some(format!("materials/{}", name).replace("//", "/"))
        }
        _ => None,
    }
}

fn texture_path<P: AsRef<str> + ?Sized>(name: &P) -> Option<String> {
    let name = name.as_ref();
    match Path::new(name).extension() {
        // We need to normalize double-slashes.
        // TODO: We should just use paths, but we need to handle Windows vs Unix path separators.
        None => Some(format!("materials/{}.vtf", name).replace("//", "/")),
        Some(ext) if ext == OsStr::new("vtf") => Some(format!("materials/{}", name)),
        _ => None,
    }
}

async fn load_texture<'a>(
    bsp: &Bsp,
    load_context: &mut LoadContext<'a>,
    name: &str,
) -> anyhow::Result<(Handle<Image>, VtfInfo)> {
    let path = texture_path(&name).unwrap_or_else(|| name.to_string());
    let Ok(data) = read_vpk_file(bsp, load_context, &path).await else {
        bail!("no such texture: {:?}", path);
    };
    let vtf_file = vtf::from_bytes(&data).expect("bad vtf");
    let header_width = vtf_file.header.width;
    let header_height = vtf_file.header.height;
    let flags = vtf_file.header.flags;
    let format = format!("{:?}", vtf_file.header.highres_image_format);
    let mut image = vtf_file.highres_image.decode(0)?;
    let vtf_info = VtfInfo {
        width: header_width,
        height: header_height,
        decoded_width: image.width(),
        decoded_height: image.height(),
        flags,
        format,
    };

    // Fixup skybox orientations
    if name.contains("skybox") {
        image = image.fliph();
        if name.contains("up") {
            image = image.rotate270();
        }
        image = image.crop_imm(1, 1, 510, 510);
    };

    let mut texture = if image.width() == 0 || image.height() == 0 {
        Image::new(
            Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            TextureDimension::D2,
            vec![0; 4],
            TextureFormat::Rgba8UnormSrgb,
            RenderAssetUsages::RENDER_WORLD,
        )
    } else {
        Image::from_dynamic(image, true, RenderAssetUsages::RENDER_WORLD)
    };

    if name.contains("skybox") {
        texture.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
            anisotropy_clamp: 16,
            address_mode_u: ImageAddressMode::ClampToBorder,
            address_mode_v: ImageAddressMode::ClampToBorder,
            ..ImageSamplerDescriptor::linear()
        });
    } else {
        texture.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
            anisotropy_clamp: 16,
            address_mode_u: ImageAddressMode::Repeat,
            address_mode_v: ImageAddressMode::Repeat,
            ..ImageSamplerDescriptor::linear()
        });
    }

    Ok((load_context.add_labeled_asset(path, texture), vtf_info))
}

#[instrument(skip(bsp, load_context))]
async fn read_vpk_file(
    bsp: &Bsp,
    load_context: &mut LoadContext<'_>,
    path: &str,
) -> anyhow::Result<Vec<u8>> {
    let base_path = AssetPath::default().with_source("vpk").into_owned();
    let asset_path = base_path.resolve(path)?;
    if let Ok(data) = load_context.read_asset_bytes(asset_path).await {
        Ok(data)
    } else if let Ok(Some(data)) = bsp.pack.get(path) {
        Ok(data)
    } else {
        bail!("file not found: {}", path);
    }
}
