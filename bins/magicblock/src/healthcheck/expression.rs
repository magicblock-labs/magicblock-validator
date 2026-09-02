use rand::Rng;
use v42_calculator_interface::builder::Expr;

const MAX_TERM: i64 = 4;

pub(super) fn random() -> Expr {
    let mut rng = rand::rng();
    let mut expr = Expr::lit(term(&mut rng)).cpi();

    // Even eight multiplications peak at 4^9, so every intermediate fits i64.
    for index in 0..rng.random_range(4..=8) {
        let op = Op::random(&mut rng);
        let mut rhs = Expr::lit(op.term(&mut rng));
        if index == 0 || rng.random() {
            rhs = rhs.cpi();
        }
        expr = op.compose(expr, rhs);
    }

    expr
}

#[derive(Clone, Copy)]
enum Op {
    Add,
    Subtract,
    Multiply,
    Divide,
}

impl Op {
    fn random(rng: &mut impl Rng) -> Self {
        match (rng.random(), rng.random()) {
            (false, false) => Self::Add,
            (false, true) => Self::Subtract,
            (true, false) => Self::Multiply,
            (true, true) => Self::Divide,
        }
    }

    fn term(self, rng: &mut impl Rng) -> i64 {
        match self {
            Self::Divide => {
                let value = rng.random_range(1..=MAX_TERM);
                if rng.random() { value } else { -value }
            }
            _ => term(rng),
        }
    }

    fn compose(self, lhs: Expr, rhs: Expr) -> Expr {
        match self {
            Self::Add => lhs + rhs,
            Self::Subtract => lhs - rhs,
            Self::Multiply => lhs * rhs,
            Self::Divide => lhs / rhs,
        }
    }
}

fn term(rng: &mut impl Rng) -> i64 {
    rng.random_range(-MAX_TERM..=MAX_TERM)
}
