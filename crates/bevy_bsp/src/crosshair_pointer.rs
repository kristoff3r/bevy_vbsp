//! A custom pointer type that always points at the center of the screen.
//!
//! This replaces the default mouse pointer for picking when the cursor is locked,
//! because messing around with the cursor pointer is very error prone. Could
//! probably be upstreamed or turned into a mini crate.

use bevy::asset::uuid::Uuid;
use bevy::camera::RenderTarget;
use bevy::input::ButtonState;
use bevy::picking::PickingSystems;
use bevy::picking::pointer::{
    Location, PointerAction, PointerButton, PointerId, PointerInput, PointerLocation,
};
use bevy::prelude::*;
use bevy::window::{CursorGrabMode, CursorOptions, PrimaryWindow, WindowEvent};

const CROSSHAIR_POINTER_ID: PointerId =
    PointerId::Custom(Uuid::from_u128(0xee0706576db04edc8fea7ed163196e4b));

/// Marker component for the crosshair pointer entity.
#[derive(Component)]
pub struct CrosshairPointer;

pub struct CrosshairPointerPlugin;

impl Plugin for CrosshairPointerPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, manage_crosshair_pointer)
            .add_systems(
                First,
                (update_crosshair_pointer, forward_mouse_buttons)
                    .run_if(any_with_component::<CrosshairPointer>)
                    .after(PickingSystems::Input)
                    .before(PickingSystems::PostInput),
            );
    }
}

fn manage_crosshair_pointer(
    mut commands: Commands,
    cursor_options: Query<&CursorOptions, (With<PrimaryWindow>, Changed<CursorOptions>)>,
    existing: Query<Entity, With<CrosshairPointer>>,
) {
    let Ok(cursor_options) = cursor_options.single() else {
        return;
    };
    match cursor_options.grab_mode {
        CursorGrabMode::None => {
            for entity in &existing {
                commands.entity(entity).despawn();
            }
        }
        _ => {
            if existing.is_empty() {
                commands.spawn((CrosshairPointer, CROSSHAIR_POINTER_ID));
            }
        }
    }
}

fn update_crosshair_pointer(
    primary_window: Query<(Entity, &Window), With<PrimaryWindow>>,
    camera: Query<&RenderTarget, With<Camera3d>>,
    mut pointer_input: MessageWriter<PointerInput>,
    pointer_query: Query<&PointerLocation, With<CrosshairPointer>>,
) {
    let Ok((window_entity, window)) = primary_window.single() else {
        return;
    };
    let Ok(render_target) = camera.single() else {
        return;
    };
    let Some(target) = render_target.normalize(Some(window_entity)) else {
        return;
    };

    let position = Vec2::new(window.width() / 2.0, window.height() / 2.0);

    let previous = pointer_query
        .single()
        .ok()
        .and_then(|loc| loc.location.as_ref())
        .map(|l| l.position)
        .unwrap_or(position);

    pointer_input.write(PointerInput::new(
        CROSSHAIR_POINTER_ID,
        Location { target, position },
        PointerAction::Move {
            delta: position - previous,
        },
    ));
}

fn forward_mouse_buttons(
    mut window_events: MessageReader<WindowEvent>,
    pointer_query: Query<&PointerLocation, With<CrosshairPointer>>,
    mut pointer_input: MessageWriter<PointerInput>,
) {
    let Some(location) = pointer_query
        .single()
        .ok()
        .and_then(|loc| loc.location.clone())
    else {
        return;
    };

    for event in window_events.read() {
        if let WindowEvent::MouseButtonInput(input) = event {
            let button = match input.button {
                MouseButton::Left => PointerButton::Primary,
                MouseButton::Right => PointerButton::Secondary,
                MouseButton::Middle => PointerButton::Middle,
                _ => continue,
            };
            let action = match input.state {
                ButtonState::Pressed => PointerAction::Press(button),
                ButtonState::Released => PointerAction::Release(button),
            };
            pointer_input.write(PointerInput::new(
                CROSSHAIR_POINTER_ID,
                location.clone(),
                action,
            ));
        }
    }
}
