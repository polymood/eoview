//! Band math: an expression of band names, for example `(B08 - B04) / (B08 + B04)`.
//! The parser makes a tree. The tree gives WGSL for the composite shader (the GPU computes the values)
//! and a CPU value for the inspector and the automatic stretch.
//!
//! Grammar: numbers, band names, + - * / ^ (power), unary -, parentheses, and the functions
//! abs sqrt ln log10 exp sin cos min max pow atan2 clamp.

#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    Num(f64),
    /// Index of the input.
    Var(usize),
    Neg(Box<Node>),
    Bin(char, Box<Node>, Box<Node>),
    Call(String, Vec<Node>),
}

const FUNCS: &[(&str, usize)] =
    &[("abs", 1), ("sqrt", 1), ("ln", 1), ("log10", 1), ("exp", 1), ("sin", 1), ("cos", 1), ("min", 2), ("max", 2), ("pow", 2), ("atan2", 2), ("clamp", 3)];

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Num(f64),
    Id(String),
    Op(char),
}

fn lex(s: &str) -> Result<Vec<Tok>, String> {
    let c: Vec<char> = s.chars().collect();
    let (mut i, mut out) = (0, vec![]);
    while i < c.len() {
        let ch = c[i];
        if ch.is_whitespace() {
            i += 1;
        } else if ch.is_ascii_digit() || ch == '.' {
            let st = i;
            while i < c.len() && (c[i].is_ascii_digit() || c[i] == '.' || ((c[i] == 'e' || c[i] == 'E') && i + 1 < c.len()) || ((c[i] == '-' || c[i] == '+') && matches!(c[i - 1], 'e' | 'E'))) {
                i += 1;
            }
            let t: String = c[st..i].iter().collect();
            out.push(Tok::Num(t.parse().map_err(|_| format!("bad number {t}"))?));
        } else if ch.is_alphabetic() || ch == '_' {
            let st = i;
            while i < c.len() && (c[i].is_alphanumeric() || c[i] == '_') {
                i += 1;
            }
            out.push(Tok::Id(c[st..i].iter().collect()));
        } else if "+-*/^(),".contains(ch) {
            out.push(Tok::Op(ch));
            i += 1;
        } else {
            return Err(format!("unexpected character '{ch}'"));
        }
    }
    Ok(out)
}

struct P<'a> {
    t: Vec<Tok>,
    i: usize,
    names: &'a [String],
    used: Vec<usize>,
}

impl P<'_> {
    fn peek(&self) -> Option<&Tok> {
        self.t.get(self.i)
    }

    fn eat(&mut self, op: char) -> bool {
        if self.peek() == Some(&Tok::Op(op)) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn expr(&mut self) -> Result<Node, String> {
        let mut a = self.term()?;
        loop {
            let op = if self.eat('+') { '+' } else if self.eat('-') { '-' } else { return Ok(a) };
            a = Node::Bin(op, Box::new(a), Box::new(self.term()?));
        }
    }

    fn term(&mut self) -> Result<Node, String> {
        let mut a = self.unary()?;
        loop {
            let op = if self.eat('*') { '*' } else if self.eat('/') { '/' } else { return Ok(a) };
            a = Node::Bin(op, Box::new(a), Box::new(self.unary()?));
        }
    }

    /// Unary minus has a lower priority than the power: -2^2 is -4.
    fn unary(&mut self) -> Result<Node, String> {
        if self.eat('-') {
            return Ok(Node::Neg(Box::new(self.unary()?)));
        }
        self.eat('+');
        self.power()
    }

    fn power(&mut self) -> Result<Node, String> {
        let a = self.atom()?;
        if self.eat('^') { Ok(Node::Bin('^', Box::new(a), Box::new(self.unary()?))) } else { Ok(a) }
    }

    fn atom(&mut self) -> Result<Node, String> {
        match self.peek().cloned() {
            Some(Tok::Num(v)) => {
                self.i += 1;
                Ok(Node::Num(v))
            }
            Some(Tok::Op('(')) => {
                self.i += 1;
                let e = self.expr()?;
                if !self.eat(')') {
                    return Err("')' expected".into());
                }
                Ok(e)
            }
            Some(Tok::Id(id)) => {
                self.i += 1;
                if self.eat('(') {
                    let lid = id.to_lowercase();
                    let &(_, n) = FUNCS.iter().find(|f| f.0 == lid).ok_or_else(|| format!("unknown function {id}"))?;
                    let mut args = vec![self.expr()?];
                    while self.eat(',') {
                        args.push(self.expr()?);
                    }
                    if !self.eat(')') {
                        return Err("')' expected".into());
                    }
                    if args.len() != n {
                        return Err(format!("{id} takes {n} argument(s)"));
                    }
                    return Ok(Node::Call(lid, args));
                }
                let k = self.names.iter().position(|n| n.eq_ignore_ascii_case(&id)).ok_or_else(|| format!("unknown band {id}"))?;
                let j = match self.used.iter().position(|&u| u == k) {
                    Some(j) => j,
                    None => {
                        self.used.push(k);
                        self.used.len() - 1
                    }
                };
                Ok(Node::Var(j))
            }
            _ => Err("value expected".into()),
        }
    }
}

/// Parse expressions of the band names `names`. The result has the trees, and the bands that they use
/// (index into `names`): `Node::Var(j)` is band `used[j]`, input j of the composite.
pub fn parse(exprs: &[&str], names: &[String]) -> Result<(Vec<Node>, Vec<usize>), String> {
    let mut p = P { t: vec![], i: 0, names, used: vec![] };
    let mut out = vec![];
    for e in exprs {
        p.t = lex(e)?;
        p.i = 0;
        let n = p.expr()?;
        if p.i != p.t.len() {
            return Err(format!("unexpected text in '{e}'"));
        }
        out.push(n);
    }
    Ok((out, p.used))
}

impl Node {
    pub fn wgsl(&self) -> String {
        match self {
            Node::Num(v) => format!("{:?}", *v as f32),
            Node::Var(j) => format!("v{j}"),
            Node::Neg(a) => format!("(-{})", a.wgsl()),
            Node::Bin('^', a, b) => format!("pow({}, {})", a.wgsl(), b.wgsl()),
            Node::Bin(op, a, b) => format!("({} {op} {})", a.wgsl(), b.wgsl()),
            Node::Call(f, a) => {
                let a: Vec<String> = a.iter().map(Node::wgsl).collect();
                match f.as_str() {
                    "ln" => format!("log({})", a[0]),
                    "log10" => format!("(log({}) * 0.4342944819)", a[0]),
                    _ => format!("{f}({})", a.join(", ")),
                }
            }
        }
    }

    /// Value on the CPU. `v[j]` is the value of input j.
    pub fn eval(&self, v: &[f64]) -> f64 {
        match self {
            Node::Num(x) => *x,
            Node::Var(j) => v.get(*j).copied().unwrap_or(f64::NAN),
            Node::Neg(a) => -a.eval(v),
            Node::Bin(op, a, b) => {
                let (a, b) = (a.eval(v), b.eval(v));
                match op {
                    '+' => a + b,
                    '-' => a - b,
                    '*' => a * b,
                    '/' => a / b,
                    _ => a.powf(b),
                }
            }
            Node::Call(f, a) => {
                let a: Vec<f64> = a.iter().map(|n| n.eval(v)).collect();
                match f.as_str() {
                    "abs" => a[0].abs(),
                    "sqrt" => a[0].sqrt(),
                    "ln" => a[0].ln(),
                    "log10" => a[0].log10(),
                    "exp" => a[0].exp(),
                    "sin" => a[0].sin(),
                    "cos" => a[0].cos(),
                    "min" => a[0].min(a[1]),
                    "max" => a[0].max(a[1]),
                    "pow" => a[0].powf(a[1]),
                    "atan2" => a[0].atan2(a[1]),
                    _ => a[0].clamp(a[1].min(a[2]), a[2].max(a[1])),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ndvi() {
        let names: Vec<String> = ["B02", "B04", "B08"].map(String::from).to_vec();
        let (t, used) = parse(&["(b08 - B04) / (B08 + B04)"], &names).unwrap();
        assert_eq!(used, [2, 1]);
        assert_eq!(t[0].wgsl(), "((v0 - v1) / (v0 + v1))");
        assert!((t[0].eval(&[0.5, 0.1]) - 0.4 / 0.6).abs() < 1e-12);
        let (t, _) = parse(&["-2^2 + max(B02, 3) * 1e-1"], &names).unwrap();
        assert!((t[0].eval(&[5.0]) - (-4.0 + 0.5)).abs() < 1e-12);
        assert!(parse(&["B05"], &names).is_err());
        assert!(parse(&["(B02"], &names).is_err());
    }
}
