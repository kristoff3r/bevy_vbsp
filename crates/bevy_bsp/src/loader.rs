use std::{
    ffi::OsStr,
    ops::Deref,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::bail;
use bevy::{
    asset::{AssetLoader, AssetPath, LoadContext, RenderAssetUsages, io::Reader},
    image::{ImageAddressMode, ImageSampler, ImageSamplerDescriptor},
    platform::collections::HashMap,
    prelude::*,
    render::render_resource::{Extent3d, TextureDimension, TextureFormat},
};
use serde::{Deserialize, Serialize};
use tracing::instrument;
use vbsp::{Angles, Bsp, GenericEntity};

use crate::SOURCE_TO_BEVY;

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
    pub spawn_points: Vec<Transform>,
}

impl BspAsset {
    /// A texture's Source `$surfaceprop` — the material name ("concrete",
    /// "wood", "metal", …) that `scripts/surfaceproperties*.txt` keys its
    /// physical properties off. Lowercased, because [`vmt_parser::from_str`]
    /// lowercases the whole VMT.
    ///
    /// `None` when the material declares none. That is normal rather than
    /// exceptional: `decals/`, `tools/` and editor textures never set it, and
    /// [`vmt_parser::material::Material::surface_prop`] only reads the key off
    /// the shaders that can carry it (notably not `VertexLitGeneric`, which is
    /// props — use [`vmdl::Model::surface_prop`] for those). Callers want a
    /// fallback material, not a warning.
    pub fn surface_prop(&self, texture_name: &str) -> Option<&str> {
        self.vmt_materials.get(texture_name)?.surface_prop()
    }
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
                    && let Ok((texture, vtf)) =
                        load_texture(&bsp, load_context, &texture_name).await
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
        let mut spawn_points = Vec::new();
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
                let mut transform = Transform::from_matrix(SOURCE_TO_BEVY.into())
                    * Transform::from_translation(origin).with_rotation(quat);
                transform.scale = Vec3::ONE;
                match entity.class.as_str() {
                    "info_player_terrorist" => {
                        spawn_points.push(transform);
                    }
                    "info_player_counterterrorist" => {
                        spawn_points.push(transform);
                    }
                    // `info_player_logo` is used in `test_hardware` in CS:S
                    "info_player_start" | "info_player_teamspawn" | "info_player_logo" => {
                        spawn_points.push(transform)
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
            spawn_points,
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
    let asset_path = base_path.resolve_str(path)?;
    if let Ok(data) = load_context.read_asset_bytes(asset_path).await {
        Ok(data)
    } else if let Ok(Some(data)) = bsp.pack.get(path) {
        Ok(data)
    } else {
        bail!("file not found: {}", path);
    }
}
