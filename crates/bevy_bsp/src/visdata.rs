//! BSP visibility (PVS) culling.
//!
//! Built once at map load from the BSP's vis lump: [`VisRoot`] (on the
//! worldspawn root entity) holds a flattened copy of the BSP node tree plus
//! the decoded per-cluster PVS bitsets, and every world-face mesh carries the
//! [`VisClusters`] it belongs to. At runtime each camera's cluster is found by
//! descending the node tree (a handful of plane tests against the camera
//! position in BSP-local space) and geometry visibility is only re-evaluated
//! when a camera crosses into a different cluster.
//!
//! Nothing here spawns entities: the PVS is bitsets, the tree is arrays, and
//! per-face membership is a short slice of cluster ids.
//!
//! Cameras opt in with [`CameraRenderMask`]; faces in the camera's PVS are
//! toggled onto that render layer. Dynamic entities opt in with
//! [`CalculateVisleaf`], which re-derives their [`VisClusters`] from their
//! world AABB whenever they move.

use bevy::math::{
    Vec3A,
    bounding::{Aabb3d, BoundingVolume as _},
};
use bevy::{
    app::{Last, Plugin, PostUpdate},
    camera::{
        Camera3d,
        primitives::Aabb,
        visibility::{Layer, RenderLayers, Visibility},
    },
    ecs::{
        change_detection::{DetectChanges as _, Ref},
        component::Component,
        entity::Entity,
        query::{Changed, Has, Or, With, Without},
        schedule::IntoScheduleConfigs as _,
        system::{Commands, Query},
    },
    transform::{TransformSystems, components::GlobalTransform},
};

/// Flattened BSP vis data, inserted on the worldspawn root entity (the one
/// carrying the Source→Bevy transform, so tree queries run in BSP-local
/// coordinates).
#[derive(Component)]
pub struct VisRoot {
    /// `children[0]` is the front side. Negative child = `!child` is a leaf
    /// index (Source convention).
    nodes: Box<[VisNode]>,
    /// Leaf index → cluster id (-1 = solid/no cluster).
    leaf_clusters: Box<[i32]>,
    head_node: i32,
    cluster_count: u32,
    /// Words per PVS row.
    stride: usize,
    /// `cluster_count` rows of `stride` words; bit `c` of row `r` = cluster
    /// `c` is potentially visible from cluster `r`.
    pvs: Box<[u64]>,
}

struct VisNode {
    normal: Vec3A,
    dist: f32,
    children: [i32; 2],
}

impl VisRoot {
    pub fn from_bsp(bsp: &vbsp::Bsp) -> Self {
        let nodes = bsp
            .nodes
            .iter()
            .map(|node| {
                let plane = &bsp.planes[node.plane_index as usize];
                VisNode {
                    normal: Vec3A::new(plane.normal.x, plane.normal.y, plane.normal.z),
                    dist: plane.dist,
                    children: node.children,
                }
            })
            .collect();

        let leaf_clusters = bsp.leaves.iter().map(|leaf| leaf.cluster).collect();

        let cluster_count = bsp.vis_data.cluster_count;
        let stride = (cluster_count as usize).div_ceil(64);
        let mut pvs = vec![0u64; cluster_count as usize * stride].into_boxed_slice();

        for cluster in 0..cluster_count {
            let row = &mut pvs[cluster as usize * stride..][..stride];

            // A cluster always sees itself; the vis lump usually encodes this
            // but be explicit.
            row[cluster as usize / 64] |= 1 << (cluster % 64);

            for visible in bsp.vis_data.visible_clusters(cluster) {
                if visible < cluster_count {
                    row[visible as usize / 64] |= 1 << (visible % 64);
                }
            }
        }

        let head_node = bsp
            .models
            .first()
            .map(|model| model.head_node)
            .unwrap_or_default();

        Self {
            nodes,
            leaf_clusters,
            head_node,
            cluster_count,
            stride,
            pvs,
        }
    }

    /// Cluster containing `point` (BSP-local coordinates), or -1 when the
    /// point is in solid space / outside the tree.
    pub fn cluster_for_point(&self, point: Vec3A) -> i32 {
        let mut idx = self.head_node;

        loop {
            if idx < 0 {
                return self
                    .leaf_clusters
                    .get(!idx as usize)
                    .copied()
                    .unwrap_or(-1);
            }

            let Some(node) = self.nodes.get(idx as usize) else {
                return -1;
            };

            idx = if node.normal.dot(point) - node.dist >= 0.0 {
                node.children[0]
            } else {
                node.children[1]
            };
        }
    }

    /// All clusters whose leaves overlap `aabb` (BSP-local coordinates),
    /// appended to `out` (deduplicated, unsorted).
    pub fn clusters_for_aabb(&self, aabb: Aabb3d, out: &mut Vec<u32>) {
        // Membership queries sit exactly on node planes (faces *are* the
        // boundary geometry, and leaf bounds are stored as truncated i16 in
        // the BSP), so pad the plane test: straddling both sides is merely
        // conservative, missing a side culls wrongly.
        const PLANE_EPSILON: f32 = 1.0;

        let center = aabb.center();
        let half = aabb.half_size();
        let start = out.len();

        let mut stack = vec![self.head_node];

        while let Some(idx) = stack.pop() {
            if idx < 0 {
                if let Some(&cluster) = self.leaf_clusters.get(!idx as usize)
                    && let Ok(cluster) = u32::try_from(cluster)
                    && !out[start..].contains(&cluster)
                {
                    out.push(cluster);
                }
                continue;
            }

            let Some(node) = self.nodes.get(idx as usize) else {
                continue;
            };

            let s = node.normal.dot(center) - node.dist;
            let r = node.normal.abs().dot(half) + PLANE_EPSILON;

            if s >= -r {
                stack.push(node.children[0]);
            }
            if s <= r {
                stack.push(node.children[1]);
            }
        }
    }

    /// Is anything in `clusters` potentially visible from `from`?
    ///
    /// Fails open: an unknown viewpoint (solid space, no vis data) or an
    /// empty membership list renders everything, mirroring Source.
    pub fn visible_from(&self, from: i32, clusters: &[u32]) -> bool {
        let Ok(from) = u32::try_from(from) else {
            return true;
        };
        if from >= self.cluster_count || clusters.is_empty() {
            return true;
        }

        let row = &self.pvs[from as usize * self.stride..][..self.stride];

        clusters
            .iter()
            .any(|&c| c < self.cluster_count && row[c as usize / 64] & (1 << (c % 64)) != 0)
    }
}

/// PVS membership of a renderable entity: the clusters it (potentially)
/// occupies in `root`'s BSP tree. Empty membership or a placeholder root
/// means "always visible".
///
/// World faces get this at map load; dynamic entities tagged
/// [`CalculateVisleaf`] get it re-derived from their AABB when they move.
#[derive(Component)]
pub struct VisClusters {
    pub root: Entity,
    pub clusters: Box<[u32]>,
}

/// Opts a camera into PVS culling: faces in this camera's PVS are toggled
/// onto this render layer (which is also added to the camera's
/// `RenderLayers`).
#[derive(Component)]
pub struct CameraRenderMask(pub Layer);

/// Debug: freeze this camera's cluster at its current value, so you can fly
/// out and inspect what the PVS actually culls.
#[derive(Component)]
pub struct LockViscluster;

/// Debug: render everything for this camera regardless of the PVS.
#[derive(Component)]
pub struct DisableVisibility;

/// Marks a dynamic (moving) entity for per-frame cluster tracking; requires
/// an [`Aabb`]. Static world faces don't need this — their clusters are
/// assigned at map load.
#[derive(Component)]
pub struct CalculateVisleaf;

/// Per-camera cache of the last evaluated view state; visibility is only
/// re-evaluated when this changes.
#[derive(Component, Default, PartialEq, Clone)]
pub struct CameraVis {
    /// Cluster per vis root (BSP world), sorted by root entity.
    pub clusters: Vec<(Entity, i32)>,
    pub disabled: bool,
}

pub struct VisdataPlugin;

impl Plugin for VisdataPlugin {
    fn build(&self, app: &mut bevy::app::App) {
        app.add_systems(
            PostUpdate,
            (ensure_camera_has_render_mask, calculate_visible_set)
                .chain()
                .after(TransformSystems::Propagate),
        )
        .add_systems(Last, update_dynamic_clusters);
    }
}

fn ensure_camera_has_render_mask(
    mut commands: Commands,
    with_layers: Query<(&CameraRenderMask, &mut RenderLayers), Changed<RenderLayers>>,
    without_layers: Query<(Entity, &CameraRenderMask), Without<RenderLayers>>,
) {
    for (mask, mut layers) in with_layers {
        if !layers.intersects(&RenderLayers::layer(mask.0)) {
            let new_layers = std::mem::take(&mut *layers).with(mask.0);
            *layers = new_layers;
        }
    }

    for (ent, mask) in without_layers {
        commands.entity(ent).insert(RenderLayers::layer(mask.0));
    }
}

fn calculate_visible_set(
    mut commands: Commands,
    cameras: Query<
        (
            Entity,
            &GlobalTransform,
            &CameraRenderMask,
            Has<DisableVisibility>,
            Has<LockViscluster>,
            Option<&mut CameraVis>,
        ),
        With<Camera3d>,
    >,
    roots: Query<(Entity, &GlobalTransform, &VisRoot)>,
    mut elements: Query<(Ref<VisClusters>, &mut RenderLayers, &mut Visibility), Without<Camera3d>>,
) {
    struct CamState {
        layer: Layer,
        vis: CameraVis,
    }

    let mut states = Vec::new();
    let mut any_changed = false;

    for (camera_entity, transform, mask, disabled, locked, previous) in cameras {
        let locked_vis = if locked {
            previous.as_deref().cloned()
        } else {
            None
        };
        let vis = locked_vis.unwrap_or_else(|| {
            let camera_position = transform.translation_vec3a();

            let mut clusters: Vec<(Entity, i32)> = roots
                .iter()
                .map(|(root_entity, root_transform, root)| {
                    let local = root_transform
                        .affine()
                        .inverse()
                        .transform_point3a(camera_position);
                    (root_entity, root.cluster_for_point(local))
                })
                .collect();
            clusters.sort_unstable_by_key(|(entity, _)| *entity);

            CameraVis { clusters, disabled }
        });

        match previous {
            Some(mut previous) => {
                if *previous != vis {
                    any_changed = true;
                    *previous = vis.clone();
                }
            }
            None => {
                any_changed = true;
                commands.entity(camera_entity).insert(vis.clone());
            }
        }

        states.push(CamState {
            layer: mask.0,
            vis,
        });
    }

    if states.is_empty() {
        return;
    }

    let all_camera_layers = states
        .iter()
        .fold(RenderLayers::none(), |layers, state| {
            layers.with(state.layer)
        });

    elements
        .par_iter_mut()
        .for_each(|(vis_clusters, mut render_layers, mut visibility)| {
            // Re-evaluate everything when any camera's view state changed;
            // otherwise only entities whose membership changed (dynamic
            // movers, freshly loaded maps).
            if !any_changed && !vis_clusters.is_changed() {
                return;
            }

            let mut new_layers = render_layers.clone();

            for state in &states {
                let visible = state.vis.disabled
                    || match state
                        .vis
                        .clusters
                        .binary_search_by_key(&vis_clusters.root, |(entity, _)| *entity)
                    {
                        Ok(i) => {
                            let (root_entity, camera_cluster) = state.vis.clusters[i];
                            roots
                                .get(root_entity)
                                .map(|(_, _, root)| {
                                    root.visible_from(camera_cluster, &vis_clusters.clusters)
                                })
                                .unwrap_or(true)
                        }
                        // The entity's root isn't a live vis root (map
                        // unloading, placeholder root): fail open.
                        Err(_) => true,
                    };

                new_layers = if visible {
                    new_layers.with(state.layer)
                } else {
                    new_layers.without(state.layer)
                };
            }

            if *render_layers != new_layers {
                *render_layers = new_layers.clone();
            }

            let new_visibility = if new_layers.intersects(&all_camera_layers) {
                Visibility::Inherited
            } else {
                Visibility::Hidden
            };

            if *visibility != new_visibility {
                *visibility = new_visibility;
            }
        });
}

/// Re-derive [`VisClusters`] for moving entities from their world-space AABB.
fn update_dynamic_clusters(
    mut commands: Commands,
    roots: Query<(Entity, &GlobalTransform, &VisRoot)>,
    movers: Query<
        (
            Entity,
            &Aabb,
            &GlobalTransform,
            Option<&mut VisClusters>,
        ),
        (
            With<CalculateVisleaf>,
            Or<(Changed<GlobalTransform>, Without<VisClusters>)>,
        ),
    >,
) {
    for (entity, aabb, transform, previous) in movers {
        let mut clusters = Vec::new();
        let mut root_entity = Entity::PLACEHOLDER;

        for (candidate_root, root_transform, root) in roots.iter() {
            // World AABB corners into BSP-local space, re-boxed. Conservative
            // under rotation, exact for the pure scale/axis-swap map root.
            let world = transform.affine();
            let local = root_transform.affine().inverse() * world;
            let local_aabb = Aabb3d::from_point_cloud(
                bevy::math::Isometry3d::IDENTITY,
                corners(aabb).map(|corner| local.transform_point3(corner)),
            );

            let before = clusters.len();
            root.clusters_for_aabb(local_aabb, &mut clusters);

            if clusters.len() > before {
                root_entity = candidate_root;
            }
        }

        let new = VisClusters {
            root: root_entity,
            clusters: clusters.into_boxed_slice(),
        };

        match previous {
            Some(mut previous) => {
                if previous.root != new.root || previous.clusters != new.clusters {
                    *previous = new;
                }
            }
            None => {
                commands.entity(entity).insert(new);
            }
        }
    }
}

fn corners(aabb: &Aabb) -> impl Iterator<Item = bevy::math::Vec3> + '_ {
    (0..8).map(|i| {
        let signs = bevy::math::Vec3::new(
            if i & 1 == 0 { -1.0 } else { 1.0 },
            if i & 2 == 0 { -1.0 } else { 1.0 },
            if i & 4 == 0 { -1.0 } else { 1.0 },
        );
        bevy::math::Vec3::from(aabb.center) + bevy::math::Vec3::from(aabb.half_extents) * signs
    })
}
