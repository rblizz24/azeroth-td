//! Tower-defense rules layered onto the original Survivors run: routed enemy waves, a defended
//! objective, and player-placed sentries. The world, unit attachment, spell combat, classes and
//! map selection remain the original game's systems.

use bevy::prelude::*;
use benilla_world::world_point::WorldPoint;

use super::abilities::Hit;
use super::data;
use super::enemies::Enemy;
use super::hero::{self, AimPoint, Hero};
use super::units::{self, UnitSpec};
use super::{Phase, Run, RunEntity, Selection};

/// The next waypoint for a creep following the route toward the objective.
#[derive(Component, Clone)]
pub(super) struct RouteFollower {
    pub next: usize,
}

/// Health for the structure the run is defending.
#[derive(Resource)]
pub(super) struct Objective {
    pub hp: f32,
    pub max_hp: f32,
    pub position: Vec3,
}

impl Default for Objective {
    fn default() -> Self {
        Self { hp: 1000.0, max_hp: 1000.0, position: Vec3::ZERO }
    }
}

#[derive(Resource)]
pub(super) struct Economy {
    pub gold: u32,
}
impl Default for Economy {
    fn default() -> Self { Self { gold: 200 } }
}

/// A placed sentry uses the existing unit attachment pipeline so it renders in the same world.
#[derive(Component)]
pub(super) struct Tower {
    pub cooldown: f32,
    pub range: f32,
    pub damage: f32,
}

const TOWER_COST: u32 = 50;
const TOWER_RANGE: f32 = 17.0;
const TOWER_DAMAGE: f32 = 28.0;

/// Generate a map-specific serpentine route in the local play area. Rotating/mirroring the
/// layout by map index gives each selected battleground a different approach while staying
/// anchored to that map's actual WoW coordinates.
pub(super) fn route_points(map_index: usize, map: &data::MapDef, world: &WorldPoint) -> Vec<Vec3> {
    let center = hero::arena(map);
    let scale = (map.leash / 80.0).clamp(0.55, 1.15);
    let mirror = if map_index % 2 == 0 { 1.0 } else { -1.0 };
    let rotate = map_index % 4;
    let local = [
        Vec2::new(0.0, 42.0),
        Vec2::new(17.0 * mirror, 30.0),
        Vec2::new(-17.0 * mirror, 17.0),
        Vec2::new(17.0 * mirror, 4.0),
        Vec2::new(-15.0 * mirror, -9.0),
        Vec2::new(0.0, -24.0),
    ];
    local.iter().map(|p| {
        let p = *p * scale;
        let rotated = match rotate {
            1 => Vec2::new(p.y, -p.x),
            2 => Vec2::new(-p.x, -p.y),
            3 => Vec2::new(-p.y, p.x),
            _ => p,
        };
        let mut point = center + Vec3::new(rotated.x, 0.0, rotated.y);
        point.y = world.terrain_height_under(point).unwrap_or(center.y);
        point
    }).collect()
}

/// Place a sentry with T at the terrain point under the cursor. Avoids placing on the creep lane
/// or directly on the objective. The model is attached by the same WoW unit pipeline as the hero.
pub(super) fn place_tower(
    mut commands: Commands,
    keys: Res<ButtonInput<KeyCode>>,
    aim: Res<AimPoint>,
    selection: Res<Selection>,
    mut run: ResMut<Run>,
    mut economy: ResMut<Economy>,
    objective: Res<Objective>,
    world: WorldPoint,
    existing: Query<&Transform, With<Tower>>,
) {
    if !keys.just_pressed(KeyCode::KeyT) || !matches!(aim.0, Some(_)) || economy.gold < TOWER_COST {
        return;
    }
    let pos = aim.0.unwrap();
    let center = hero::arena(selection.stage_def());
    if units::flat(pos - center).length() > selection.stage_def().leash * 0.85
        || units::flat(pos - objective.position).length() < 5.0
        || existing.iter().any(|tf| units::flat(tf.translation - pos).length() < 4.0)
    {
        return;
    }
    let route = route_points(selection.stage, selection.stage_def(), &world);
    if route.iter().any(|p| units::flat(*p - pos).length() < 4.0) {
        return;
    }
    let Some(class) = selection.class_def() else { return };
    let display = selection.looks.get(selection.look.unwrap_or(0)).map(|v| v.0)
        .unwrap_or(class.looks[0]);
    let serial = run.guid();
    let e = units::spawn_unit(
        &mut commands,
        &UnitSpec {
            serial,
            display,
            scale: 1.0,
            level: 1,
            faction: units::FACTION_FRIENDLY,
            bytes0: 0,
            weapons: &[],
            sheath: 0,
        },
        pos,
        0.0,
    );
    commands.entity(e).insert(Tower { cooldown: 0.1, range: TOWER_RANGE, damage: TOWER_DAMAGE });
    economy.gold -= TOWER_COST;
}

/// Sentries automatically attack the nearest creep in range through the game's existing hit
/// pipeline, preserving enemy debuffs, damage handling, death effects and XP drops.
pub(super) fn tower_fire(
    time: Res<Time>,
    mut towers: Query<(&Transform, &mut Tower)>,
    enemies: Query<(Entity, &Transform, &Enemy)>,
    mut hits: MessageWriter<Hit>,
) {
    let dt = time.delta_secs();
    for (tower_tf, mut tower) in &mut towers {
        tower.cooldown -= dt;
        if tower.cooldown > 0.0 { continue; }
        let target = enemies.iter()
            .filter(|(_, _, enemy)| !enemy.dead)
            .filter_map(|(entity, tf, _)| {
                let distance = units::flat(tf.translation - tower_tf.translation).length();
                (distance <= tower.range).then_some((entity, distance))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1));
        if let Some((entity, _)) = target {
            hits.write(Hit::direct(entity, tower.damage, false));
            tower.cooldown = 0.8;
        } else {
            tower.cooldown = 0.15;
        }
    }
}

/// Creeps reaching the final waypoint damage the objective instead of attacking the hero.
pub(super) fn leak_damage(
    mut commands: Commands,
    mut objective: ResMut<Objective>,
    mut run: ResMut<Run>,
    mut enemies: Query<(Entity, &Transform, &Enemy, &RouteFollower)>,
    route: Res<super::tower_defense::RouteCache>,
) {
    let Some(goal) = route.points.last().copied() else { return };
    for (entity, tf, enemy, follower) in &mut enemies {
        if enemy.dead || follower.next < route.points.len() { continue; }
        if units::flat(tf.translation - goal).length() <= 1.7 {
            objective.hp = (objective.hp - if enemy.boss { 100.0 } else { 20.0 }).max(0.0);
            commands.entity(entity).despawn();
            if objective.hp <= 0.0 {
                run.dead_t = Some(run.elapsed);
            }
        }
    }
}

/// Cached route refreshed when a run changes battleground.
#[derive(Resource, Default)]
pub(super) struct RouteCache {
    pub map_index: usize,
    pub points: Vec<Vec3>,
}

pub(super) fn refresh_route(
    mut commands: Commands,
    selection: Res<Selection>,
    world: WorldPoint,
    mut cache: ResMut<RouteCache>,
    mut objective: ResMut<Objective>,
    mut economy: ResMut<Economy>,
    spawned: Query<Entity, With<RunEntity>>,
    mut run: ResMut<Run>,
) {
    if cache.map_index == selection.stage && !cache.points.is_empty() { return; }
    // A map change starts a clean defense setup; menu preview entities are not RunEntity.
    for entity in &spawned { commands.entity(entity).despawn(); }
    cache.map_index = selection.stage;
    cache.points = route_points(selection.stage, selection.stage_def(), &world);
    objective.position = cache.points.last().copied().unwrap_or_else(|| hero::arena(selection.stage_def()));
    objective.hp = objective.max_hp;
    economy.gold = 200;
    let next_guid = run.next_guid;
    *run = Run { next_guid, ..Run::default() };
}

/// Mark the objective as failed through the same results state used by the original run.
pub(super) fn objective_failure(
    objective: Res<Objective>,
    mut next: ResMut<NextState<Phase>>,
    state: Res<State<Phase>>,
) {
    if objective.hp <= 0.0 && *state.get() == Phase::Playing {
        next.set(Phase::GameOver);
    }
}
