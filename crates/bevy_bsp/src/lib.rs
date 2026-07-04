mod astc;

pub mod crosshair_pointer;
pub mod debug;
pub mod entities;
pub mod loader;
pub mod matcher;
pub mod mesh;
pub mod util;
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

                            if let Some(collider) = processed_mdl.dynamic_collider() {
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
    /// Combined collision geometry, retained so colliders can be built lazily on first use.
    collision_mesh: Option<Mesh>,
    /// Exact triangle-mesh collider for static bodies. Cheap to build; cached on first use.
    static_collider: OnceLock<Option<Collider>>,
    /// Convex-decomposition collider for dynamic bodies. Expensive (VHACD), so it is only built
    /// for models actually placed as dynamic props, and cached on first use.
    dynamic_collider: OnceLock<Option<Collider>>,
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
            dynamic_collider: OnceLock::new(),
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

    /// Collider for a [`RigidBody::Dynamic`] placement. Trimeshes have no volume and can't drive
    /// dynamic mass/contacts, so this falls back to convex decomposition (VHACD).
    pub fn dynamic_collider(&self) -> Option<Collider> {
        self.dynamic_collider
            .get_or_init(|| {
                self.collision_mesh
                    .as_ref()
                    .and_then(Collider::convex_decomposition_from_mesh)
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
                    image: Some(image),
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
