pub mod entities;

use core::panic;
use std::{ops::Deref, path::PathBuf, result::Result, sync::Arc};

use anyhow::bail;
use avian3d::prelude::RigidBody;
use bevy::{
    asset::{AssetLoader, AssetPath, LoadContext, RenderAssetUsages, io::Reader},
    core_pipeline::Skybox,
    image::{ImageAddressMode, ImageSampler, ImageSamplerDescriptor, TextureFormatPixelInfo},
    math::primitives,
    platform::collections::HashMap,
    prelude::*,
    render::render_resource::{
        Extent3d, TextureDimension, TextureViewDescriptor, TextureViewDimension,
    },
};
use entities::spawn_bsp_model;
use serde::{Deserialize, Serialize};
use vbsp::{Angles, Bsp, GenericEntity, StaticPropLumpFlags, Vector};

use bevy_vpk::{vmt::VmtAssetLoader, vtf::VtfAssetLoader};
use tracing::instrument;

use entities::spawn_mdl_model;

pub struct BspLoaderPlugin;

pub const SCALE: f32 = 0.03125;

#[derive(Resource)]
pub struct MapAssets {
    pub bsp: Handle<BspAsset>,
}

fn source_to_bevy(v: Vector) -> [f32; 3] {
    [SCALE * v.y, SCALE * v.z, SCALE * v.x]
}

fn angles_to_bevy(angles: &Angles) -> Quat {
    Quat::from_euler(
        EulerRot::YXZ,
        angles.yaw.to_radians(),
        angles.pitch.to_radians(),
        angles.roll.to_radians(),
    )
}

impl Plugin for BspLoaderPlugin {
    fn build(&self, app: &mut App) {
        app.init_asset::<BspAsset>();
        app.init_asset_loader::<BspAssetLoader>();
        app.init_asset_loader::<VtfAssetLoader>();
        app.init_asset_loader::<VmtAssetLoader>();
    }
}

pub fn spawn_map_entities(
    mut commands: Commands,
    map_assets: Res<MapAssets>,
    mut meshes: ResMut<Assets<Mesh>>,
    bsp_asset_data: Res<Assets<BspAsset>>,
    mut images: ResMut<Assets<Image>>,
    camera: Option<Single<Entity, With<Camera>>>,
) {
    let bsp_asset = bsp_asset_data.get(&map_assets.bsp).cloned().unwrap();
    let bsp = bsp_asset.bsp.clone();

    info!("Loaded BSP models: {}", bsp.models().count());

    // for model in bsp.models() {
    //     spawn_bsp_model(&mut commands, &bsp_asset, &mut meshes, model);
    // }
    for raw_entity in &bsp.entities {
        let entity: GenericEntity = raw_entity.parse().unwrap();
        let class = entity.class.as_str();
        debug!(?class, "map entity");
        if class.starts_with("weapon") {
            debug!("{raw_entity:?}");
        }
        match class {
            "worldspawn" => {
                spawn_bsp_model(
                    &mut commands,
                    &bsp_asset,
                    &mut meshes,
                    bsp.models().next().unwrap(),
                    Transform::IDENTITY,
                );
            }
            _ => {
                if let Some(model) = entity.data.get("model") {
                    let origin = entity
                        .data
                        .get("origin")
                        .and_then(|e| e.as_value())
                        .and_then(|s| {
                            let mut parts = s.split(' ');
                            Some(Vector {
                                x: parts.next()?.parse().ok()?,
                                y: parts.next()?.parse().ok()?,
                                z: parts.next()?.parse().ok()?,
                            })
                        })
                        .unwrap_or_default();

                    let angles = entity
                        .data
                        .get("angles")
                        .and_then(|e| e.as_value())
                        .and_then(|s| {
                            let mut parts = s.split(' ');
                            Some(Angles {
                                pitch: parts.next()?.parse().ok()?,
                                yaw: parts.next()?.parse().ok()?,
                                roll: parts.next()?.parse().ok()?,
                            })
                        })
                        .unwrap_or_default();

                    let transform = Transform::from_translation(Vec3::from(source_to_bevy(origin)))
                        .with_rotation(angles_to_bevy(&angles));

                    if let Some(model) = model.as_value() {
                        if model.starts_with("*") {
                            let idx: usize = model.deref().split_at(1).1.parse().unwrap();
                            let model = bsp.models().nth(idx).unwrap();
                            // info!("Spawning model for entity {idx} faces={}", model.face_count);
                            spawn_bsp_model(
                                &mut commands,
                                &bsp_asset,
                                &mut meshes,
                                model,
                                transform,
                            );
                        } else {
                            // if let Some(model) = bsp_asset.models.get(model.deref()) {
                            //     spawn_mdl_model(
                            //         &mut commands,
                            //         &bsp_asset,
                            //         &mut meshes,
                            //         model,
                            //         transform,
                            //         RigidBody::Dynamic,
                            //     );
                            // }
                        }
                    }
                } else {
                    // println!("unknown entity: {:?}", entity.class);
                }
            }
        }
    }

    for transform in bsp_asset
        .t_spawn_points
        .iter()
        .chain(bsp_asset.ct_spawn_points.iter())
    {
        commands.spawn((
            Name::new("Spawn Point"),
            *transform,
            bsp_asset.default_material.clone(),
            Mesh3d(
                meshes.add(
                    primitives::Cuboid {
                        half_size: Vec3::splat(0.5),
                    }
                    .mesh(),
                ),
            ),
        ));
    }

    for static_prop in bsp.static_props() {
        if static_prop.flags.contains(StaticPropLumpFlags::NO_DRAW) {
            continue;
        }

        let name = bsp.static_props.dict.name[static_prop.prop_type as usize]
            .as_str()
            .to_ascii_lowercase();

        let model = bsp_asset.models.get(name.as_str()).unwrap_or_else(|| {
            panic!(
                "static prop model={} not found in bsp pakfile",
                name.as_str()
            )
        });

        let transform = Transform::from_translation(Vec3::from(source_to_bevy(static_prop.origin)))
            .with_rotation(angles_to_bevy(&static_prop.angles));

        spawn_mdl_model(
            &mut commands,
            &bsp_asset,
            &mut meshes,
            model,
            transform,
            RigidBody::Static,
        );
    }

    if bsp_asset.skybox_images.len() == 6 {
        let (size, format) = {
            let image = images.get(&bsp_asset.skybox_images[0]).unwrap();

            (image.size(), image.texture_descriptor.format)
        };
        let pixel_size = format.pixel_size().unwrap() as u32;
        // let (size, format) = (UVec2::splat(512), TextureFormat::Rgba8UnormSrgb);
        // println!("Skybox size and format: {size:?} {format:?} pixel_size={pixel_size}");
        let mut result = Image::new(
            Extent3d {
                width: size.x,
                height: size.y,
                depth_or_array_layers: 6,
            },
            TextureDimension::D2,
            vec![0xff; (size.x * size.y * pixel_size * 6) as usize],
            format,
            RenderAssetUsages::default(),
        );
        for (i, handle) in bsp_asset.skybox_images.iter().enumerate() {
            let image = images.get(handle).unwrap();
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

        // let image = {
        //     let image = images.get_mut(&bsp_asset.cubemap).unwrap();
        //     image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        //         // anisotropy_clamp: 1,
        //         // address_mode_u: ImageAddressMode::Repeat,
        //         // address_mode_v: ImageAddressMode::Repeat,
        //         ..ImageSamplerDescriptor::linear()
        //     });
        //     image.reinterpret_stacked_2d_as_array(6);
        //     image.texture_view_descriptor = Some(TextureViewDescriptor {
        //         dimension: Some(TextureViewDimension::Cube),
        //         ..default()
        //     });

        //     bsp_asset.cubemap.clone()
        // };
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
}

#[derive(Default, TypePath)]
pub struct BspAssetLoader;

#[derive(Asset, TypePath, Clone)]
pub struct BspAsset {
    pub bsp: Arc<vbsp::Bsp>,
    pub materials: Arc<HashMap<String, Handle<StandardMaterial>>>,
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

        let default_texture: Handle<Image> = load_context.load("images/UVCheckerMap01-512.png");
        // let default_texture: Handle<Image> = load_context.load("TestPattern.png");
        let cubemap: Handle<Image> = load_context.load("images/labeled_skybox.png");
        let default_material = StandardMaterial {
            base_color_texture: Some(default_texture.clone()),
            perceptual_roughness: 0.8,
            reflectance: 0.2,
            metallic: 0.0,
            ..default()
        };

        let load_material = async |load_context: &mut LoadContext<'_>, name: &str| {
            let path = material_path(name);
            let data = read_vpk_file(&bsp, load_context, &path).await?;
            let vmt = String::from_utf8(data).expect("bad vmt utf8");
            let Ok(mut vmt) = vmt_parser::from_str(&vmt) else {
                bail!("bad vmt: {}", path);
            };

            if let vmt_parser::material::Material::Patch(mat) = vmt {
                let include_path = mat.include.to_lowercase();
                let base =
                    String::from_utf8(read_vpk_file(&bsp, load_context, &include_path).await?)
                        .expect("bad vmt utf8")
                        .to_ascii_lowercase();

                vmt = mat.apply(&base).expect("bad vmt patch");
            }

            let texture = if let Some(name) = vmt.base_texture() {
                if let Ok(texture) = load_texture(&bsp, load_context, &name).await {
                    Some(texture)
                } else {
                    warn!("Using default texture for missing texture: {}", name);
                    Some(default_texture.clone())
                }
            } else {
                // warn!("No base texture for material: {vmt:?}");
                Some(default_texture.clone())
            };

            let bump_map = if let Some(name) = vmt.bump_map() {
                load_texture(&bsp, load_context, &name).await.ok()
            } else {
                None
            };

            let base_color = match &vmt {
                vmt_parser::material::Material::UnlitGeneric(mat) => {
                    Color::srgba(mat.color.0[0], mat.color.0[1], mat.color.0[2], mat.alpha)
                }
                _ => Color::WHITE,
            };

            let material = StandardMaterial {
                base_color,
                base_color_texture: texture,
                normal_map_texture: bump_map,
                perceptual_roughness: 0.8,
                reflectance: 0.2,
                metallic: 0.0,
                alpha_mode: if vmt.translucent() {
                    AlphaMode::Blend
                } else if let Some(test) = vmt.alpha_test() {
                    AlphaMode::Mask(test)
                } else {
                    AlphaMode::Opaque
                },
                ..default()
            };

            Ok::<_, anyhow::Error>(material)
        };

        let default_material = load_context
            .add_labeled_asset("default".into(), default_material)
            .into();

        for texture in bsp.textures() {
            let name = texture.name().to_ascii_lowercase();
            if materials.contains_key(&name) {
                continue;
            }

            // println!("Loading material: {}", name);

            let material = load_material(load_context, &name).await?;

            let material_load_context = load_context.begin_labeled_asset();
            let asset = material_load_context.finish(material);

            let mat_handle =
                load_context.add_loaded_labeled_asset::<StandardMaterial>(format!("{name}"), asset);

            materials.insert(name.to_owned(), mat_handle.clone());
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
                    for search_path in &texture.search_paths {
                        let name = format!(
                            "{}{}",
                            search_path.to_ascii_lowercase(),
                            texture.name.to_ascii_lowercase()
                        );
                        if materials.contains_key(&name) {
                            continue;
                        }
                        let Ok(material) = load_material(load_context, &name).await else {
                            continue;
                        };
                        let material_load_context = load_context.begin_labeled_asset();
                        let asset = material_load_context.finish(material);
                        let mat_handle = load_context
                            .add_loaded_labeled_asset::<StandardMaterial>(format!("{name}"), asset);
                        materials.insert(name.to_owned(), mat_handle.clone());
                        break 'outer;
                    }

                    warn!("No material found for model texture: {}", texture.name);
                }
            };

        let mut models = HashMap::new();
        let mut t_spawn_points = Vec::new();
        let mut ct_spawn_points = Vec::new();
        for entity in &bsp.entities {
            let entity: GenericEntity = entity.parse().unwrap();
            if let Some(model) = entity.data.get("model") {
                if let Some(model_key) = model.as_value() {
                    let model_key = model_key.deref();
                    if !model_key.starts_with("*") && !model_key.ends_with("vmt") {
                        if models.contains_key(model_key) {
                            continue;
                        }
                        let Ok(model_data) = load_model(load_context, model_key).await else {
                            continue;
                        };

                        load_model_textures(load_context, &model_data).await;

                        models.insert(model_key.to_owned(), model_data);
                    }
                }
            }
            if entity.class.starts_with("info_player") {
                // println!("Found spawn point: {:?}", entity);
                let origin = entity
                    .data
                    .get("origin")
                    .and_then(|e| e.as_value())
                    .and_then(|s| {
                        let mut parts = s.split(' ');
                        Some(Vector {
                            x: parts.next()?.parse().ok()?,
                            y: parts.next()?.parse().ok()?,
                            z: parts.next()?.parse().ok()?,
                        })
                    })
                    .unwrap_or_default();

                let angles = entity
                    .data
                    .get("angles")
                    .and_then(|e| e.as_value())
                    .and_then(|s| {
                        let mut parts = s.split(' ');
                        Some(Angles {
                            pitch: parts.next()?.parse().ok()?,
                            yaw: parts.next()?.parse().ok()?,
                            roll: parts.next()?.parse().ok()?,
                        })
                    })
                    .unwrap_or_default();

                let transform = Transform::from_translation(Vec3::from(source_to_bevy(origin)))
                    .with_rotation(angles_to_bevy(&angles));
                match entity.class.as_str() {
                    "info_player_terrorist" => {
                        t_spawn_points.push(transform);
                    }
                    "info_player_counterterrorist" => {
                        ct_spawn_points.push(transform);
                    }
                    "info_player_start" => (),
                    _ => {
                        panic!("unknown class: {}", entity.class);
                    }
                }
            }
        }

        for model in &bsp.static_props.dict.name {
            let model_key = model.as_str().to_ascii_lowercase();
            if models.contains_key(&model_key) {
                continue;
            }
            let model_data = load_model(load_context, &model_key)
                .await
                .unwrap_or_else(|e| {
                    panic!("model={model_key:?} not found in vpk or bsp pakfile: {e}")
                });

            load_model_textures(load_context, &model_data).await;

            models.insert(model_key.to_owned(), model_data);
        }

        let worldspawn: GenericEntity = bsp.entities.iter().next().unwrap().parse().unwrap();

        let skybox = worldspawn
            .data
            .get("skyname")
            .and_then(|e| e.as_value())
            .unwrap()
            .to_ascii_lowercase();

        let mut skybox_images = Vec::new();
        for dir in ["rt", "lf", "up", "dn", "ft", "bk"] {
            let path = format!("skybox/{skybox}{dir}");
            match load_texture(&bsp, load_context, &path).await {
                Ok(image) => {
                    // println!("Loaded skybox image: {}", path);
                    skybox_images.push(image);
                }
                Err(e) => {
                    println!("Missing skybox image {}: {e}", path);
                }
            }
        }

        Ok(BspAsset {
            bsp: Arc::new(bsp),
            materials: Arc::new(materials),
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

fn material_path(name: &str) -> String {
    format!("materials/{}.vmt", name)
}

fn texture_path(name: &str) -> String {
    format!("materials/{}.vtf", name)
}

async fn load_texture<'a>(
    bsp: &Bsp,
    load_context: &mut LoadContext<'a>,
    name: &str,
) -> anyhow::Result<Handle<Image>> {
    // todo: why no worky
    // let mut load_context = load_context.begin_labeled_asset();
    // let path_str = texture_path(name);
    // let path = AssetPath::from_path(&PathBuf::from(&path_str))
    //     .with_source("vpk")
    //     .into_owned();

    // let image = load_context
    //     .loader()
    //     .with_static_type()
    //     .immediate()
    //     .load(&path)
    //     .await?;

    // Ok(load_context.add_loaded_labeled_asset(path_str, image))

    let path = texture_path(&name);
    let Ok(data) = read_vpk_file(&bsp, load_context, &path).await else {
        warn!("No such texture: {}", path);
        bail!("no such texture: {}", path);
    };
    let image = vtf::from_bytes(&data).expect("bad vtf");
    let mut image = image.highres_image.decode(0)?;

    // Fixup skybox orientations
    if name.contains("skybox") {
        image = image.fliph();
        if name.contains("up") {
            image = image.rotate270();
        }
        image = image.crop_imm(1, 1, 510, 510);
    };

    let mut texture = Image::from_dynamic(image, true, RenderAssetUsages::default());

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

    Ok(load_context.add_labeled_asset(path, texture))
}

#[instrument(skip(bsp, load_context))]
async fn read_vpk_file(
    bsp: &Bsp,
    load_context: &mut LoadContext<'_>,
    path: &str,
) -> anyhow::Result<Vec<u8>> {
    let base_path = AssetPath::default().with_source("vpk").into_owned();
    let asset_path = base_path.resolve(path).unwrap();
    if let Ok(data) = load_context.read_asset_bytes(asset_path).await {
        Ok(data)
    } else if let Ok(Some(data)) = bsp.pack.get(&path) {
        Ok(data)
    } else {
        bail!("file not found: {}", path);
    }
}
