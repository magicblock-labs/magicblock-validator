use crate::TimeDelta;

/// Example physics game state showing time delta usage
#[derive(Debug, Clone)]
pub struct PlayerState {
    pub x: f32,
    pub y: f32,
    pub velocity_x: f32,
    pub velocity_y: f32,
    pub time_tracker: TimeDelta,
}

impl PlayerState {
    /// Create a new player at origin with 10ms physics tick interval
    pub fn new() -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            velocity_x: 0.0,
            velocity_y: 0.0,
            time_tracker: TimeDelta::new(10), // 10ms tick interval for physics
        }
    }

    /// Create a new player with custom tick interval
    pub fn new_with_interval(tick_interval_ms: u64) -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            velocity_x: 0.0,
            velocity_y: 0.0,
            time_tracker: TimeDelta::new(tick_interval_ms),
        }
    }

    /// Apply gravity if enough time has elapsed
    /// Returns the number of physics ticks that were processed
    pub fn apply_gravity(&mut self, gravity: f32) -> u64 {
        if !self.time_tracker.should_tick() {
            return 0; // Not enough time elapsed
        }

        let tick_count = self.time_tracker.tick_count();
        let tick_duration_seconds = self.time_tracker.tick_interval() as f32 / 1000.0;

        // Apply gravity for each tick that has elapsed
        for _ in 0..tick_count {
            self.velocity_y += gravity * tick_duration_seconds;
            self.y += self.velocity_y * tick_duration_seconds;
            self.x += self.velocity_x * tick_duration_seconds;
        }

        // Update the time tracker
        self.time_tracker.update();
        
        tick_count
    }

    /// Apply movement without gravity (for horizontal movement)
    pub fn apply_movement(&mut self) -> u64 {
        if !self.time_tracker.should_tick() {
            return 0;
        }

        let tick_count = self.time_tracker.tick_count();
        let tick_duration_seconds = self.time_tracker.tick_interval() as f32 / 1000.0;

        // Apply movement for each tick
        for _ in 0..tick_count {
            self.x += self.velocity_x * tick_duration_seconds;
            self.y += self.velocity_y * tick_duration_seconds;
        }

        self.time_tracker.update();
        tick_count
    }

    /// Set velocity (e.g., from user input)
    pub fn set_velocity(&mut self, vx: f32, vy: f32) {
        self.velocity_x = vx;
        self.velocity_y = vy;
    }

    /// Check if player can perform an action (rate limiting)
    pub fn can_perform_action(&self, cooldown_ms: u64) -> bool {
        self.time_tracker.elapsed_ms() >= cooldown_ms
    }

    /// Get current position
    pub fn position(&self) -> (f32, f32) {
        (self.x, self.y)
    }

    /// Get current velocity
    pub fn velocity(&self) -> (f32, f32) {
        (self.velocity_x, self.velocity_y)
    }

    /// Get time until next physics tick
    pub fn time_to_next_tick(&self) -> u64 {
        self.time_tracker.time_to_next_tick()
    }
}

impl Default for PlayerState {
    fn default() -> Self {
        Self::new()
    }
}

/// Physics world that manages multiple players
#[derive(Debug)]
pub struct PhysicsWorld {
    players: Vec<PlayerState>,
    gravity: f32,
}

impl PhysicsWorld {
    /// Create a new physics world with specified gravity
    pub fn new(gravity: f32) -> Self {
        Self {
            players: Vec::new(),
            gravity,
        }
    }

    /// Add a player to the world
    pub fn add_player(&mut self, player: PlayerState) -> usize {
        self.players.push(player);
        self.players.len() - 1 // Return player index
    }

    /// Update all players in the world
    pub fn update(&mut self) -> Vec<u64> {
        self.players
            .iter_mut()
            .map(|player| player.apply_gravity(self.gravity))
            .collect()
    }

    /// Get player by index
    pub fn get_player(&self, index: usize) -> Option<&PlayerState> {
        self.players.get(index)
    }

    /// Get mutable player by index
    pub fn get_player_mut(&mut self, index: usize) -> Option<&mut PlayerState> {
        self.players.get_mut(index)
    }

    /// Get number of players
    pub fn player_count(&self) -> usize {
        self.players.len()
    }

    /// Set gravity for the world
    pub fn set_gravity(&mut self, gravity: f32) {
        self.gravity = gravity;
    }

    /// Get current gravity
    pub fn gravity(&self) -> f32 {
        self.gravity
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn test_player_creation() {
        let player = PlayerState::new();
        assert_eq!(player.position(), (0.0, 0.0));
        assert_eq!(player.velocity(), (0.0, 0.0));
        assert_eq!(player.time_tracker.tick_interval(), 10);
    }

    #[test]
    fn test_player_with_custom_interval() {
        let player = PlayerState::new_with_interval(25);
        assert_eq!(player.time_tracker.tick_interval(), 25);
    }

    #[test]
    fn test_gravity_application() {
        let mut player = PlayerState::new();
        
        // No gravity should be applied immediately
        let ticks = player.apply_gravity(-9.8);
        assert_eq!(ticks, 0);
        
        // Wait for one tick interval
        thread::sleep(Duration::from_millis(15));
        
        // Now gravity should be applied
        let ticks = player.apply_gravity(-9.8);
        assert!(ticks >= 1);
        
        // Player should have moved down due to gravity
        let (_, y) = player.position();
        assert!(y < 0.0); // Should have fallen down
        
        // Velocity should have changed
        let (_, vy) = player.velocity();
        assert!(vy < 0.0); // Should be moving downward
    }

    #[test]
    fn test_movement_without_gravity() {
        let mut player = PlayerState::new();
        player.set_velocity(10.0, 0.0); // Move right
        
        // Wait for physics tick
        thread::sleep(Duration::from_millis(15));
        
        let ticks = player.apply_movement();
        assert!(ticks >= 1);
        
        // Player should have moved right
        let (x, y) = player.position();
        assert!(x > 0.0);
        assert_eq!(y, 0.0); // No vertical movement
    }

    #[test]
    fn test_rate_limiting() {
        let player = PlayerState::new();
        
        // Should be able to perform action immediately
        assert!(player.can_perform_action(0));
        
        // Should not be able to perform action with high cooldown
        assert!(!player.can_perform_action(1000));
        
        // Wait and check again
        thread::sleep(Duration::from_millis(20));
        assert!(player.can_perform_action(15));
    }

    #[test]
    fn test_physics_world() {
        let mut world = PhysicsWorld::new(-9.8);
        
        // Add players
        let player1 = PlayerState::new();
        let player2 = PlayerState::new_with_interval(20);
        
        let index1 = world.add_player(player1);
        let index2 = world.add_player(player2);
        
        assert_eq!(world.player_count(), 2);
        assert_eq!(index1, 0);
        assert_eq!(index2, 1);
        
        // Set different velocities
        world.get_player_mut(0).unwrap().set_velocity(5.0, 2.0);
        world.get_player_mut(1).unwrap().set_velocity(-3.0, 1.0);
        
        // Wait and update
        thread::sleep(Duration::from_millis(25));
        let tick_counts = world.update();
        
        // Both players should have been updated
        assert!(tick_counts.len() == 2);
        
        // Check positions changed
        let player1_pos = world.get_player(0).unwrap().position();
        let player2_pos = world.get_player(1).unwrap().position();
        
        println!("Player 1 position: {:?}", player1_pos);
        println!("Player 2 position: {:?}", player2_pos);
        
        // Players should have moved
        assert!(player1_pos.0 != 0.0 || player1_pos.1 != 0.0);
        assert!(player2_pos.0 != 0.0 || player2_pos.1 != 0.0);
    }

    #[test]
    fn test_precise_tick_counting() {
        let mut player = PlayerState::new_with_interval(50); // 50ms intervals
        
        // Wait for approximately 2.5 tick intervals
        thread::sleep(Duration::from_millis(125));
        
        let ticks = player.apply_gravity(-10.0);
        assert_eq!(ticks, 2); // Should process exactly 2 ticks (125ms / 50ms = 2.5, truncated to 2)
        
        // Check that physics was applied for exactly 2 ticks
        let tick_duration = 0.05; // 50ms = 0.05 seconds
        let expected_velocity = -10.0 * tick_duration * 2.0; // gravity * time * ticks
        let (_, actual_velocity) = player.velocity();
        
        // Allow for small floating point differences
        assert!((actual_velocity - expected_velocity).abs() < 0.001);
    }

    #[test]
    fn test_time_to_next_tick() {
        let player = PlayerState::new_with_interval(100);
        
        // Initially should be close to the full interval
        let time_to_next = player.time_to_next_tick();
        assert!(time_to_next <= 100);
        assert!(time_to_next > 90);
        
        // Wait and check again
        thread::sleep(Duration::from_millis(30));
        let time_to_next = player.time_to_next_tick();
        assert!(time_to_next <= 70);
        assert!(time_to_next >= 60);
    }

    #[test]
    fn test_default_implementation() {
        let player: PlayerState = Default::default();
        assert_eq!(player.position(), (0.0, 0.0));
        assert_eq!(player.velocity(), (0.0, 0.0));
    }

    #[test]
    fn test_multiple_gravity_applications() {
        let mut player = PlayerState::new_with_interval(20);
        
        // Apply gravity multiple times
        thread::sleep(Duration::from_millis(25));
        let ticks1 = player.apply_gravity(-9.8);
        
        let pos1 = player.position();
        let vel1 = player.velocity();
        
        thread::sleep(Duration::from_millis(25));
        let ticks2 = player.apply_gravity(-9.8);
        
        let pos2 = player.position();
        let vel2 = player.velocity();
        
        // Both should have processed ticks
        assert!(ticks1 > 0);
        assert!(ticks2 > 0);
        
        // Position and velocity should have changed further
        assert!(pos2.1 < pos1.1); // Fallen further
        assert!(vel2.1 < vel1.1); // Velocity increased (more negative)
    }
}