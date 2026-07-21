//! Validates `VisRoot`'s BSP-tree descent and PVS decode against a real map.
//! Skips silently when no CS:S install is present.
#![cfg(feature = "visdata")]

use bevy::math::{Vec3A, bounding::Aabb3d};
use bevy_bsp::visdata::VisRoot;

fn load_dust2() -> Option<vbsp::Bsp> {
    let steam_common = std::env::var("CSFORCE_STEAM_COMMON").unwrap_or_else(|_| {
        format!(
            "{}/.steam/steam/steamapps/common/Counter-Strike Source",
            std::env::var("HOME").unwrap()
        )
    });
    let path = format!("{steam_common}/cstrike/maps/de_dust2.bsp");

    let data = std::fs::read(path).ok()?;
    Some(vbsp::Bsp::read(&data).expect("de_dust2.bsp should parse"))
}

fn vec3a(v: impl Into<[f32; 3]>) -> Vec3A {
    Vec3A::from(v.into())
}

#[test]
fn visroot_matches_leaf_geometry() {
    let Some(bsp) = load_dust2() else {
        eprintln!("skipping: no CS:S install found");
        return;
    };

    let root = VisRoot::from_bsp(&bsp);

    // Only leaves under the worldspawn head node participate in vis; other
    // models (doors, breakables) hang under their own head nodes.
    let mut reachable = vec![false; bsp.leaves.iter().count()];
    let mut stack = vec![bsp.models.first().unwrap().head_node];
    while let Some(idx) = stack.pop() {
        if idx < 0 {
            reachable[!idx as usize] = true;
        } else {
            stack.extend(bsp.nodes[idx as usize].children);
        }
    }

    let clustered_leaves: Vec<_> = bsp
        .leaves
        .iter()
        .enumerate()
        .filter(|(i, leaf)| reachable[*i] && leaf.cluster >= 0)
        .map(|(_, leaf)| leaf)
        .collect();
    assert!(
        clustered_leaves.len() > 100,
        "dust2 should have plenty of vis leaves, got {}",
        clustered_leaves.len()
    );

    // Descending to the AABB center of a leaf should land in that leaf's
    // cluster for the overwhelming majority of leaves (centers of highly
    // concave leaf volumes may fall outside — that's fine, but a systematic
    // plane-side/sign error would fail nearly all of them).
    let mut hits = 0;
    for leaf in &clustered_leaves {
        let center = (vec3a(leaf.mins.to_array()) + vec3a(leaf.maxs.to_array())) / 2.0;
        if root.cluster_for_point(center) == leaf.cluster {
            hits += 1;
        }
    }
    let ratio = hits as f32 / clustered_leaves.len() as f32;
    assert!(
        ratio > 0.8,
        "only {hits}/{} leaf centers landed in their own cluster",
        clustered_leaves.len()
    );

    // An AABB query over a leaf's own bounds must include its cluster.
    let mut clusters = Vec::new();
    let sample_size = clustered_leaves.len().min(500);
    let mut contained = 0;
    for leaf in clustered_leaves.iter().take(sample_size) {
        clusters.clear();
        root.clusters_for_aabb(
            Aabb3d {
                min: vec3a(leaf.mins.to_array()),
                max: vec3a(leaf.maxs.to_array()),
            },
            &mut clusters,
        );
        if clusters.contains(&(leaf.cluster as u32)) {
            contained += 1;
        }
    }
    assert_eq!(contained, sample_size);

    // Every cluster sees itself; PVS culls *something* (a broken decode that
    // returns "everything visible" would still pass the checks above).
    let sample = clustered_leaves[clustered_leaves.len() / 2].cluster;
    assert!(root.visible_from(sample, &[sample as u32]));

    let some_invisible = clustered_leaves.iter().any(|leaf| {
        leaf.cluster != sample && !root.visible_from(sample, &[leaf.cluster as u32])
    });
    assert!(
        some_invisible,
        "PVS from cluster {sample} claims every cluster is visible"
    );

    // Unknown viewpoints and unknown membership fail open.
    assert!(root.visible_from(-1, &[sample as u32]));
    assert!(root.visible_from(sample, &[]));
}
