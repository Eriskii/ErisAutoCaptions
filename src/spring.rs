/// A damped spring, set up as SwiftUI's are: `response` is the period of the undamped
/// oscillation in seconds and `damping` the damping fraction, 1 being critical. Retargeting
/// keeps the velocity, so an interrupted motion turns around instead of starting over.
#[derive(Clone, Copy, Debug)]
pub struct Spring {
    pub value: f64,
    pub target: f64,
    pub velocity: f64,
    pub response: f64,
    pub damping: f64,
    /// How close to the target, in the value's own units, counts as there.
    pub epsilon: f64,
}

impl Spring {
    pub const fn new(value: f64, response: f64, damping: f64) -> Self {
        Self { value, target: value, velocity: 0.0, response, damping, epsilon: 0.0005 }
    }

    pub const fn with_epsilon(mut self, epsilon: f64) -> Self {
        self.epsilon = epsilon;
        self
    }

    pub fn is_resting(&self) -> bool {
        self.value == self.target && self.velocity == 0.0
    }

    pub fn animate(&mut self, target: f64) {
        self.target = target;
    }

    /// Jumps to `value` and stays there.
    pub fn set(&mut self, value: f64) {
        self.value = value;
        self.target = value;
        self.velocity = 0.0;
    }

    pub fn settle(&mut self) {
        self.set(self.target);
    }

    pub fn step(&mut self, dt: f64) {
        if self.is_resting() {
            return;
        }
        let stiffness = (std::f64::consts::TAU / self.response).powi(2);
        let friction = 4.0 * std::f64::consts::PI * self.damping / self.response;
        let mut left = dt;
        while left > 0.0 {
            let h = left.min(1.0 / 600.0);
            self.velocity += (-stiffness * (self.value - self.target) - friction * self.velocity) * h;
            self.value += self.velocity * h;
            left -= h;
        }
        if (self.value - self.target).abs() < self.epsilon && self.velocity.abs() < self.epsilon * 20.0 {
            self.settle();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(spring: &mut Spring, seconds: f64) -> f64 {
        let mut peak = spring.value;
        for _ in 0..(seconds * 120.0) as usize {
            spring.step(1.0 / 120.0);
            peak = peak.max(spring.value);
        }
        peak
    }

    #[test]
    fn comes_to_rest_on_its_target() {
        let mut spring = Spring::new(0.0, 0.4, 0.6);
        spring.animate(1.0);
        run(&mut spring, 3.0);
        assert!(spring.is_resting());
        assert_eq!(spring.value, 1.0);
    }

    #[test]
    fn underdamped_overshoots_and_critical_does_not() {
        let mut bouncy = Spring::new(0.0, 0.4, 0.6);
        bouncy.animate(1.0);
        let peak = run(&mut bouncy, 1.0);
        // exp(-ζπ/√(1-ζ²)) ≈ 9.5% for ζ = 0.6.
        assert!((peak - 1.095).abs() < 0.01, "peak {peak}");

        let mut critical = Spring::new(0.0, 0.4, 1.0);
        critical.animate(1.0);
        assert!(run(&mut critical, 1.0) <= 1.0);
    }

    #[test]
    fn retargeting_keeps_the_velocity() {
        let mut spring = Spring::new(0.0, 0.4, 0.8);
        spring.animate(1.0);
        run(&mut spring, 0.1);
        let velocity = spring.velocity;
        spring.animate(0.0);
        assert_eq!(spring.velocity, velocity);
        spring.step(1.0 / 120.0);
        assert!(spring.velocity > 0.0, "still moving up just after turning");
    }

    #[test]
    fn a_resting_spring_does_not_move() {
        let mut spring = Spring::new(0.5, 0.4, 0.6);
        spring.step(1.0);
        assert_eq!(spring.value, 0.5);
        assert!(spring.is_resting());
    }
}
