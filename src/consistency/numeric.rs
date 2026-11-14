//! Exact comparison of explicitly comparable numeric requirements.

use std::cmp::Ordering;

/// A decimal represented as `coefficient / 10^scale`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decimal {
    pub coefficient: i128,
    pub scale: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operator {
    Equal,
    AtLeast,
    GreaterThan,
    AtMost,
    LessThan,
}

/// A conversion is expressed as a decimal multiplier to a named common unit.
/// Equal names are comparable only when the caller has established their identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unit {
    pub dimension: String,
    pub multiplier: Decimal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Constraint {
    pub value: Decimal,
    pub operator: Operator,
    pub unit: Unit,
}

impl Decimal {
    fn multiply(self, other: Self) -> Option<Self> {
        Some(
            Self {
                coefficient: self.coefficient.checked_mul(other.coefficient)?,
                scale: self.scale.checked_add(other.scale)?,
            }
            .normalized(),
        )
    }

    fn normalized(mut self) -> Self {
        if self.coefficient == 0 {
            self.scale = 0;
        } else {
            while self.scale > 0 && self.coefficient % 10 == 0 {
                self.coefficient /= 10;
                self.scale -= 1;
            }
        }
        self
    }

    fn compare(self, other: Self) -> Option<Ordering> {
        let a = self.normalized();
        let b = other.normalized();
        let scale = a.scale.max(b.scale);
        let a = a
            .coefficient
            .checked_mul(10_i128.checked_pow(scale - a.scale)?)?;
        let b = b
            .coefficient
            .checked_mul(10_i128.checked_pow(scale - b.scale)?)?;
        Some(a.cmp(&b))
    }
}

/// Returns whether two constraints overlap, or `None` when they cannot safely
/// be compared (unknown unit relationship or arithmetic overflow).
pub fn overlaps(a: &Constraint, b: &Constraint) -> Option<bool> {
    if a.unit.dimension.is_empty()
        || a.unit.dimension != b.unit.dimension
        || a.unit.multiplier.coefficient <= 0
        || b.unit.multiplier.coefficient <= 0
    {
        return None;
    }
    let av = a.value.multiply(a.unit.multiplier)?;
    let bv = b.value.multiply(b.unit.multiplier)?;
    let order = av.compare(bv)?;
    use Operator::*;
    Some(match (a.operator, b.operator) {
        (Equal, Equal) => order == Ordering::Equal,
        (Equal, AtLeast) | (AtMost, Equal) => order != Ordering::Less,
        (Equal, GreaterThan) | (LessThan, Equal) => order == Ordering::Greater,
        (Equal, AtMost) | (AtLeast, Equal) => order != Ordering::Greater,
        (Equal, LessThan) | (GreaterThan, Equal) => order == Ordering::Less,
        (AtLeast, AtMost) => order != Ordering::Greater,
        (AtLeast, LessThan) | (GreaterThan, AtMost) | (GreaterThan, LessThan) => {
            order == Ordering::Less
        }
        (AtMost, AtLeast) => order != Ordering::Less,
        (AtMost, GreaterThan) | (LessThan, AtLeast) | (LessThan, GreaterThan) => {
            order == Ordering::Greater
        }
        _ => true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(value: i128, scale: u32, op: Operator, dimension: &str, multiplier: i128) -> Constraint {
        Constraint {
            value: Decimal {
                coefficient: value,
                scale,
            },
            operator: op,
            unit: Unit {
                dimension: dimension.into(),
                multiplier: Decimal {
                    coefficient: multiplier,
                    scale: 0,
                },
            },
        }
    }

    #[test]
    fn compares_exact_values_and_open_boundaries() {
        use Operator::*;
        let kg = |n, op| c(n, 0, op, "mass", 1);
        assert_eq!(overlaps(&kg(300, Equal), &kg(500, Equal)), Some(false));
        assert_eq!(
            overlaps(&c(5, 1, Equal, "mass", 1000), &kg(500, Equal)),
            Some(true)
        );
        assert_eq!(overlaps(&kg(300, AtLeast), &kg(500, Equal)), Some(true));
        assert_eq!(
            overlaps(&kg(300, GreaterThan), &kg(300, AtMost)),
            Some(false)
        );
        assert_eq!(overlaps(&kg(300, AtLeast), &kg(300, AtMost)), Some(true));
        assert_eq!(
            overlaps(&kg(300, Equal), &c(300, 0, Equal, "length", 1)),
            None
        );
        assert_eq!(
            overlaps(&kg(i128::MAX, Equal), &c(1, 1, Equal, "mass", 1)),
            None
        );
    }
    #[test]
    fn retains_every_independent_claim_and_requires_mandatory_status() {
        use crate::consistency::claims::{
            Claim, Constraint as ClaimConstraint, Grounded, Object, Operator as ClaimOperator,
        };
        use crate::consistency::markdown::Evidence;
        let grounded = |text: &str| Grounded {
            text: text.into(),
            evidence: Evidence {
                version: "v".into(),
                block_id: 0,
                range: 0..1,
            },
        };
        let claim = |value: &str, mandatory: Option<&str>| Claim {
            statement: grounded(value),
            object: Object {
                key: Some("object".into()),
                evidence: vec![grounded("object")],
                alternatives: vec![],
            },
            property: grounded("mass"),
            constraint: ClaimConstraint::Numeric {
                operator: ClaimOperator::Exact,
                operator_evidence: grounded("ровно"),
                value: grounded(value),
                unit: Some(grounded("кг")),
            },
            stage: None,
            conditions: vec![],
            mandatory: mandatory.map(grounded),
            complete: true,
        };
        let a = claim("300", Some("должен"));
        let b = claim("500", Some("должен"));
        let c = claim("500", Some("должен"));
        let d = claim("700", None);
        assert_eq!(
            hypotheses(&[&a, &b, &c, &d]),
            vec![Hypothesis {
                claims: vec![0, 1, 2]
            }]
        );
        let mut nominal = d.clone();
        if let ClaimConstraint::Numeric {
            operator,
            operator_evidence,
            value,
            ..
        } = &mut nominal.constraint
        {
            *operator = ClaimOperator::TableNominal;
            *operator_evidence = value.clone();
        }
        assert_eq!(
            hypotheses(&[&a, &nominal]),
            vec![Hypothesis { claims: vec![0, 1] }]
        );
        nominal.mandatory = Some(grounded("может"));
        assert!(hypotheses(&[&a, &nominal]).is_empty());
        nominal.mandatory = None;
        if let ClaimConstraint::Numeric {
            operator_evidence, ..
        } = &mut nominal.constraint
        {
            *operator_evidence = grounded("ровно");
        }
        assert!(hypotheses(&[&a, &nominal]).is_empty());
    }
}

/// One group of incompatible requirements. Indices refer to the input slice;
/// the corresponding claims retain all individually validated evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hypothesis {
    pub claims: Vec<usize>,
}

/// Compare claims whose object, property, conditions and applicable stages have
/// already been established as the same by the caller from grounded evidence.
/// Unknown mandatory status, units and arithmetic overflow cannot prove conflict.
pub fn hypotheses(claims: &[&super::claims::Claim]) -> Vec<Hypothesis> {
    let constraints: Vec<_> = claims
        .iter()
        .map(|claim| {
            use super::claims::{Constraint as ClaimConstraint, Operator as ClaimOperator};
            if !claim.complete {
                return None;
            }
            let ClaimConstraint::Numeric {
                operator,
                operator_evidence,
                value,
                unit: Some(unit),
            } = &claim.constraint
            else {
                return None;
            };
            if *operator == ClaimOperator::TableNominal && operator_evidence != value {
                return None;
            }
            match claim.mandatory.as_ref() {
                None if *operator == ClaimOperator::TableNominal => {}
                Some(mandatory)
                    if matches!(
                        mandatory.text.trim().to_lowercase().as_str(),
                        "обязательно"
                            | "обязателен"
                            | "обязательна"
                            | "обязательное"
                            | "должен"
                            | "должна"
                            | "должно"
                            | "должны"
                            | "shall"
                            | "must"
                            | "требуется"
                    ) => {}
                _ => return None,
            }
            let operator = match operator {
                ClaimOperator::Exact | ClaimOperator::TableNominal => Operator::Equal,
                ClaimOperator::AtLeast => Operator::AtLeast,
                ClaimOperator::MoreThan => Operator::GreaterThan,
                ClaimOperator::AtMost => Operator::AtMost,
                ClaimOperator::LessThan => Operator::LessThan,
            };
            Some(Constraint {
                value: parse_decimal(&value.text)?,
                operator,
                unit: parse_unit(&unit.text)?,
            })
        })
        .collect();
    let mut groups = std::collections::BTreeMap::<&str, Vec<usize>>::new();
    for i in 0..claims.len() {
        for j in i + 1..claims.len() {
            if let (Some(a), Some(b)) = (&constraints[i], &constraints[j])
                && overlaps(a, b) == Some(false)
            {
                let involved = groups.entry(&a.unit.dimension).or_default();
                involved.push(i);
                involved.push(j);
            }
        }
    }
    groups
        .into_values()
        .map(|mut claims| {
            claims.sort_unstable();
            claims.dedup();
            Hypothesis { claims }
        })
        .collect()
}

pub(super) fn parse_decimal(text: &str) -> Option<Decimal> {
    let text = text.trim();
    let (sign, digits) = match text.as_bytes().first() {
        Some(b'-') => (-1_i128, &text[1..]),
        Some(b'+') => (1, &text[1..]),
        _ => (1, text),
    };
    let mut parts = digits.split(['.', ',']);
    let whole = parts.next()?;
    let fraction = parts.next();
    if whole.is_empty()
        || !whole.bytes().all(|c| c.is_ascii_digit())
        || parts.next().is_some()
        || fraction.is_some_and(|f| f.is_empty() || !f.bytes().all(|c| c.is_ascii_digit()))
    {
        return None;
    }
    let fraction = fraction.unwrap_or("");
    let scale = u32::try_from(fraction.len()).ok()?;
    let magnitude: i128 = format!("{whole}{fraction}").parse().ok()?;
    Some(
        Decimal {
            coefficient: magnitude.checked_mul(sign)?,
            scale,
        }
        .normalized(),
    )
}

pub(super) fn parse_unit(text: &str) -> Option<Unit> {
    let (dimension, multiplier) = match text.trim().to_lowercase().as_str() {
        "кг" | "kg" => ("mass", 1),
        "т" | "t" => ("mass", 1000),
        "г" | "g" => {
            return Some(Unit {
                dimension: "mass".into(),
                multiplier: Decimal {
                    coefficient: 1,
                    scale: 3,
                },
            });
        }
        "мм" | "mm" => ("length_mm", 1),
        "см" | "cm" => ("length_mm", 10),
        "м" | "m" => ("length_mm", 1000),
        "суток" | "сутки" | "сут" | "дней" | "дня" | "день" | "day" | "days" => {
            ("days", 1)
        }
        _ => return None,
    };
    Some(Unit {
        dimension: dimension.into(),
        multiplier: Decimal {
            coefficient: multiplier,
            scale: 0,
        },
    })
}
