mod astc;

pub mod crosshair_pointer;
pub mod debug;
pub mod entities;
pub mod loader;
pub mod matcher;
pub mod mesh;
pub mod visdata;

use std::{ops::Deref, sync::OnceLock};

use astc::{astc_convert, extents};
use avian3d::prelude::{Collider, RigidBody};
use bevy::{
    asset::RenderAssetUsages,
    core_pipeline::Skybox,
    image::TextureFormatPixelInfo,
    math::Affine3A,
    pbr::Lightmap,
    platform::collections::{HashMap, hash_map::Entry},
    prelude::*,
    render::render_resource::{
        AstcBlock, Extent3d, TextureDimension, TextureViewDescriptor, TextureViewDimension,
    },
};
use image::{Rgba32FImage, imageops::FilterType};
use itertools::Either;
pub use loader::{BspAsset, BspAssetLoader, BspSettings, VtfInfo};
use mesh::spawn_bsp_model;
use qbsp::{
    data::LightmapStyle,
    mesh::lightmap::{DefaultLightmapPacker, PerStyleLightmapData},
};
use vbsp::{Angles, EntityProp, StaticPropLumpFlags};

use bevy_vpk::{vmt::VmtAssetLoader, vtf::VtfAssetLoader};

use entities::{BspEntityModelMesh, BspStaticPropMesh};
use mesh::{spawn_mdl_model, spawn_worldspawn};

// Re-export everything while we use a lot of git dependencies
pub use bevy_vpk;
pub use qbsp;
pub use vbsp;
pub use vdf_reader;
pub use vmdl;
pub use vmt_parser;
pub use vpk;

use crate::{
    matcher::{AnyString, Not, StringMatcher},
    mesh::FaceSpawner,
};

pub struct BspLoaderPlugin;

pub const SCALE: f32 = 39.37008f32.recip();

#[derive(Resource)]
pub struct MapAssets {
    pub bsp: Handle<BspAsset>,
}

/// Settings that influence how BSP entities and geometry are spawned.
#[derive(Resource, Debug, Clone, Copy)]
pub struct BspSpawnSettings {
    /// [`RenderAssetUsages`] applied to BSP geometry meshes (worldspawn, brush
    /// entities, and props).
    ///
    /// Defaults to [`RenderAssetUsages::RENDER_WORLD`], which drops the mesh data
    /// from the main world after GPU upload to save memory. Mesh-picking ray-casts
    /// read vertex data on the CPU, so they cannot hit such meshes;
    /// [`debug::BspDebugPlugin`] overrides this to also include
    /// [`RenderAssetUsages::MAIN_WORLD`].
    pub mesh_usages: RenderAssetUsages,
}

impl Default for BspSpawnSettings {
    fn default() -> Self {
        Self {
            mesh_usages: RenderAssetUsages::RENDER_WORLD,
        }
    }
}

pub const SOURCE_TO_BEVY: Affine3A = Affine3A {
    matrix3: Mat3A {
        x_axis: Vec3A::new(0., 0., -SCALE),
        y_axis: Vec3A::new(-SCALE, 0., 0.),
        z_axis: Vec3A::new(0., SCALE, 0.),
    },
    translation: Vec3A::ZERO,
};

impl Plugin for BspLoaderPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, sync_skybox_cameras);

        app.init_asset::<BspAsset>()
            .init_resource::<BspSpawnSettings>()
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
                 spawn_settings: Res<BspSpawnSettings>,
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
                        entity.bsp,
                        bsp_asset,
                        &mut meshes,
                        bsp_asset.bsp.models().next().expect("No worldspawn"),
                        &bsp.styles_to_image,
                        &bsp.atlas_rects,
                        spawn_settings.mesh_usages,
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
                 spawn_settings: Res<BspSpawnSettings>,
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
                        .and_then(|s| <[f32; 3]>::parse(s).ok())
                        .map(Vec3::from_array)
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
                                entity.bsp,
                                bsp_asset,
                                &mut meshes,
                                model_handle,
                                &bsp.styles_to_image,
                                &bsp.atlas_rects,
                                transform,
                                &entity.class,
                                idx,
                                spawn_settings.mesh_usages,
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
                                            spawn_mdl_model(
                                                bsp_asset,
                                                model,
                                                spawn_settings.mesh_usages,
                                            ),
                                            &mut meshes,
                                        ))
                                    }
                                };

                            if let Some(collider) = processed_mdl.static_collider() {
                                commands.spawn((
                                    ChildOf(entity.bsp),
                                    collider,
                                    RigidBody::Static,
                                    transform,
                                ));
                            }

                            for VMdlComponent { mesh, material } in &processed_mdl.components {
                                commands.spawn((
                                    ChildOf(entity.bsp),
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

#[derive(Default, Copy, Clone, PartialEq, Eq, Debug)]
pub struct LightmapSettings {
    pub astc_block_size: Option<AstcBlock>,
}

/// The single root entity of a loaded map. Everything spawned for the map
/// (world geometry, props, colliders, BSP entity data, lighting) is a
/// descendant, so despawning this one entity unloads the whole map
/// (visibility link entities are cleaned up by relationship hooks). Also
/// carries the [`GlobalBspInfo`].
#[derive(Component, Default, Copy, Clone, Debug)]
pub struct BspMapRoot;

/// The map's `skyname` cubemap, assembled from the six `materials/skybox/*`
/// sides and hung on the [`BspMapRoot`] — so it is unloaded with the map, and
/// so *which* camera renders it isn't this crate's decision. Cameras opt in
/// with [`BspSkyboxCamera`].
#[derive(Component, Clone, Debug)]
pub struct BspSkybox {
    pub image: Handle<Image>,
}

/// Marks the camera(s) that should render the loaded map's [`BspSkybox`].
///
/// Opt-in rather than "whatever camera exists": an app has several (UI, weapon
/// overlays, render-to-texture), the skybox belongs on exactly one of them, and
/// the map loads long before the gameplay camera necessarily exists. Insert
/// this and [`sync_skybox_cameras`] keeps the [`Skybox`] component in step with
/// map loads and unloads, whichever happens first.
#[derive(Component, Default, Copy, Clone, Debug)]
pub struct BspSkyboxCamera {
    /// Passed through to [`Skybox::brightness`] (lux).
    pub brightness: f32,
}

impl BspSkyboxCamera {
    pub const DEFAULT_BRIGHTNESS: f32 = 1000.0;
}

/// Stack the six loaded skybox sides into one cube-mapped [`Image`], in the
/// order [`loader`] collected them (+X, -X, +Y, -Y, +Z, -Z, the order wgpu
/// expects the array layers in).
fn build_skybox_cubemap(
    sides: &[Handle<Image>],
    images: &mut Assets<Image>,
) -> Option<Handle<Image>> {
    const SIDES: usize = 6;

    if sides.len() != SIDES {
        if !sides.is_empty() {
            warn!("skybox has {} of {SIDES} sides, skipping", sides.len());
        }
        return None;
    }

    let mut size = UVec2::ZERO;
    let mut format = None;
    for handle in sides {
        let Some(image) = images.get(handle) else {
            warn!("skybox side is not loaded, skipping skybox");
            return None;
        };
        size = size.max(image.size());
        let side_format = image.texture_descriptor.format;
        if *format.get_or_insert(side_format) != side_format {
            warn!("mismatched texture formats in skybox, skipping");
            return None;
        }
    }

    let format = format?;
    let pixel_size = format.pixel_size().ok()? as u32;
    let size = size.max(UVec2::ONE);
    let side_bytes = (size.x * size.y * pixel_size) as usize;

    let mut cubemap = Image::new(
        Extent3d {
            width: size.x,
            height: size.y,
            depth_or_array_layers: SIDES as u32,
        },
        TextureDimension::D2,
        vec![0xff; side_bytes * SIDES],
        format,
        RenderAssetUsages::RENDER_WORLD,
    );

    for (i, handle) in sides.iter().enumerate() {
        let image = images.get(handle)?;
        let resized;
        let image = if image.size() == size {
            image
        } else {
            // Sides of a Source skybox are usually all the same size; a mismatch
            // (an HDR side, a downsampled `dn`) would leave the cubemap face
            // striped, so scale it to the largest instead.
            let dynamic = image.clone().try_into_dynamic().ok()?;
            resized = Image::from_dynamic(
                dynamic.resize_to_fill(size.x, size.y, FilterType::CatmullRom),
                true,
                RenderAssetUsages::RENDER_WORLD,
            );
            &resized
        };

        let (Some(dst), Some(src)) = (cubemap.data.as_mut(), image.data.as_ref()) else {
            warn!("skybox side has no pixel data, skipping skybox");
            return None;
        };
        if src.len() != side_bytes {
            warn!(
                "skybox side is {} bytes, expected {side_bytes}, skipping skybox",
                src.len()
            );
            return None;
        }
        dst[side_bytes * i..side_bytes * (i + 1)].copy_from_slice(src);
    }

    cubemap.texture_view_descriptor = Some(TextureViewDescriptor {
        dimension: Some(TextureViewDimension::Cube),
        ..default()
    });

    Some(images.add(cubemap))
}

/// Give every [`BspSkyboxCamera`] the loaded map's cubemap, and take it away
/// again when the map unloads.
pub fn sync_skybox_cameras(
    mut commands: Commands,
    map: Query<&BspSkybox>,
    cameras: Query<(Entity, &BspSkyboxCamera, Option<&Skybox>)>,
) {
    let wanted = map.iter().next().map(|skybox| &skybox.image);

    for (entity, settings, current) in &cameras {
        match (wanted, current) {
            (Some(image), current) if current.map(|s| s.image.as_ref()) != Some(Some(image)) => {
                let brightness = if settings.brightness > 0.0 {
                    settings.brightness
                } else {
                    BspSkyboxCamera::DEFAULT_BRIGHTNESS
                };
                commands.entity(entity).insert(Skybox {
                    image: Some(image.clone()),
                    brightness,
                    ..default()
                });
            }
            (None, Some(_)) => {
                commands.entity(entity).remove::<Skybox>();
            }
            _ => {}
        }
    }
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
    /// Combined collision geometry, retained so colliders can be built lazily on first use.
    collision_mesh: Option<Mesh>,
    /// Exact triangle-mesh collider for static bodies. Cheap to build; cached on first use.
    static_collider: OnceLock<Option<Collider>>,
}

impl ProcessedMdl {
    pub fn new<I>(components: I, meshes: &mut Assets<Mesh>) -> Self
    where
        I: IntoIterator<Item = (Mesh, Handle<StandardMaterial>)>,
    {
        // `Mesh::merge` only extends attributes that already exist on the
        // target, so merging into an empty mesh does nothing. Seed the combined
        // mesh with the first component, then merge the rest.
        let mut collision_mesh: Option<Mesh> = None;

        let components = components
            .into_iter()
            .map(|(mesh, material)| {
                match &mut collision_mesh {
                    Some(combined) => combined
                        .merge(&mesh)
                        .expect("MDL component meshes share a primitive topology"),
                    None => collision_mesh = Some(mesh.clone()),
                }
                VMdlComponent {
                    mesh: meshes.add(mesh),
                    material,
                }
            })
            .collect();

        Self {
            components,
            collision_mesh,
            static_collider: OnceLock::new(),
        }
    }

    /// Collider for a [`RigidBody::Static`] placement.
    pub fn static_collider(&self) -> Option<Collider> {
        self.static_collider
            .get_or_init(|| {
                self.collision_mesh
                    .as_ref()
                    .and_then(Collider::trimesh_from_mesh)
            })
            .clone()
    }
}

#[cfg(not(feature = "visdata"))]
type DefaultFaceSpawner = crate::mesh::GlobalFaceSpawner;

#[cfg(feature = "visdata")]
type DefaultFaceSpawner = crate::mesh::VisclusterFaceSpawner;

pub fn spawn_map_entities(
    In(lightmap_settings): In<LightmapSettings>,
    mut commands: Commands,
    map_assets: Res<MapAssets>,
    mut meshes: ResMut<Assets<Mesh>>,
    bsp_asset_data: Res<Assets<BspAsset>>,
    mut images: ResMut<Assets<Image>>,
    spawn_settings: Res<BspSpawnSettings>,
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

    // The single root everything map-related hangs off of; despawning it
    // unloads the map. It also carries the `GlobalBspInfo` (inserted at the
    // end of this function, once the lightmap/model caches are built).
    let world_root = commands
        .spawn((
            BspMapRoot,
            Name::new("BSP Map"),
            Transform::default(),
            Visibility::default(),
        ))
        .id();

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
                    spawn_mdl_model(&bsp_asset, model, spawn_settings.mesh_usages),
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

        if let Some(collider) = processed_mdl.static_collider() {
            commands.spawn((ChildOf(world_root), collider, RigidBody::Static, transform));
        }

        for (mesh, material, lightmap) in bundles {
            let mut new_entity = commands.spawn((
                ChildOf(world_root),
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

    if let Some(image) = build_skybox_cubemap(&bsp_asset.skybox_images, &mut images) {
        // The cubemap belongs to the map, not to a camera: which camera draws it
        // is the app's call (see `BspSkyboxCamera`), and hanging it here means it
        // is dropped when the map is.
        commands.entity(world_root).insert(BspSkybox { image });
    }

    commands.entity(world_root).insert(GlobalBspInfo {
        styles_to_image,
        processed_models,
        bsp: map_assets.bsp.clone(),
        atlas_rects,
    });

    commands.spawn_batch(
        bsp.entities
            .iter()
            .map(|raw_entity| raw_entity.parse().unwrap())
            .map(move |entity| {
                (
                    ChildOf(world_root),
                    BspEntity {
                        entity,
                        bsp: world_root,
                    },
                )
            })
            .collect::<Vec<_>>(),
    );
}
