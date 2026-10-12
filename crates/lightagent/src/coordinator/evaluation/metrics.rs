//! Pure evaluation arithmetic: no I/O, no Jev, no randomness.
//!
//! Every rate is reported with its numerator and denominator so a reader can
//! judge the sample behind it. A rate over an empty denominator is `None`,
//! never zero.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};

/// The z-score of a two-sided 95% interval.
const Z95: f64 = 1.959_963_984_540_054;

/// `numerator / denominator` with both kept, or no value when empty.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Ratio {
    pub numerator: u32,
    pub denominator: u32,
}

impl Ratio {
    pub(crate) fn new(numerator: u32, denominator: u32) -> Self {
        Self {
            numerator,
            denominator,
        }
    }

    pub(crate) fn value(self) -> Option<f64> {
        (self.denominator > 0).then(|| f64::from(self.numerator) / f64::from(self.denominator))
    }

    /// The Wilson score interval at 95%, which stays inside `[0, 1]` and is
    /// honest about small samples.
    pub(crate) fn wilson(self) -> Option<(f64, f64)> {
        if self.denominator == 0 {
            return None;
        }
        let n = f64::from(self.denominator);
        let p = f64::from(self.numerator) / n;
        let z2 = Z95 * Z95;
        let centre = (p + z2 / (2.0 * n)) / (1.0 + z2 / n);
        let half = Z95 * (p * (1.0 - p) / n + z2 / (4.0 * n * n)).sqrt() / (1.0 + z2 / n);
        Some(((centre - half).max(0.0), (centre + half).min(1.0)))
    }

    pub(crate) fn json(self) -> Value {
        json!({
            "numerator": self.numerator,
            "denominator": self.denominator,
            "value": self.value().map(round),
            "wilson_95": self.wilson().map(|(low, high)| [round(low), round(high)]),
        })
    }
}

/// Four decimal places: enough for any rate here, stable in reports.
pub(crate) fn round(value: f64) -> f64 {
    (value * 10_000.0).round() / 10_000.0
}

/// A confusion matrix over string labels, `gold -> predicted -> count`.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Confusion {
    labels: BTreeSet<String>,
    counts: BTreeMap<(String, String), u32>,
}

/// Precision, recall and F1 for one label.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ClassMetrics {
    pub precision: Ratio,
    pub recall: Ratio,
    /// Gold occurrences.
    pub support: u32,
}

impl ClassMetrics {
    pub(crate) fn f1(self) -> Option<f64> {
        let (p, r) = (self.precision.value()?, self.recall.value()?);
        Some(if p + r == 0.0 {
            0.0
        } else {
            2.0 * p * r / (p + r)
        })
    }
}

impl Confusion {
    /// A matrix that reports every label in `labels`, even unseen ones.
    pub(crate) fn with_labels<'a>(labels: impl IntoIterator<Item = &'a str>) -> Self {
        Self {
            labels: labels.into_iter().map(str::to_owned).collect(),
            counts: BTreeMap::new(),
        }
    }

    pub(crate) fn add(&mut self, gold: &str, predicted: &str) {
        self.labels.insert(gold.to_owned());
        self.labels.insert(predicted.to_owned());
        *self
            .counts
            .entry((gold.to_owned(), predicted.to_owned()))
            .or_default() += 1;
    }

    pub(crate) fn count(&self, gold: &str, predicted: &str) -> u32 {
        self.counts
            .get(&(gold.to_owned(), predicted.to_owned()))
            .copied()
            .unwrap_or(0)
    }

    pub(crate) fn total(&self) -> u32 {
        self.counts.values().sum()
    }

    pub(crate) fn correct(&self) -> u32 {
        self.labels
            .iter()
            .map(|label| self.count(label, label))
            .sum()
    }

    pub(crate) fn class(&self, label: &str) -> ClassMetrics {
        let tp = self.count(label, label);
        let predicted: u32 = self.labels.iter().map(|gold| self.count(gold, label)).sum();
        let support: u32 = self.labels.iter().map(|pred| self.count(label, pred)).sum();
        ClassMetrics {
            precision: Ratio::new(tp, predicted),
            recall: Ratio::new(tp, support),
            support,
        }
    }

    /// The unweighted mean F1 over labels that occur as gold or prediction.
    /// A label never predicted has undefined precision; its F1 counts as zero
    /// when it has gold support, which is the conservative convention.
    pub(crate) fn macro_f1(&self) -> Option<f64> {
        let scores: Vec<f64> = self
            .labels
            .iter()
            .filter_map(|label| {
                let class = self.class(label);
                let predicted = class.precision.denominator;
                if class.support == 0 && predicted == 0 {
                    None
                } else {
                    Some(class.f1().unwrap_or(0.0))
                }
            })
            .collect();
        (!scores.is_empty()).then(|| scores.iter().sum::<f64>() / scores.len() as f64)
    }

    pub(crate) fn json(&self) -> Value {
        let labels: Vec<&String> = self.labels.iter().collect();
        let rows: Vec<Vec<u32>> = labels
            .iter()
            .map(|gold| labels.iter().map(|pred| self.count(gold, pred)).collect())
            .collect();
        let per_class: BTreeMap<&String, Value> = labels
            .iter()
            .map(|label| {
                let class = self.class(label);
                (
                    *label,
                    json!({
                        "precision": class.precision.json(),
                        "recall": class.recall.json(),
                        "f1": class.f1().map(round),
                        "support": class.support,
                    }),
                )
            })
            .collect();
        json!({
            "labels": labels,
            "rows_are": "gold",
            "columns_are": "predicted",
            "matrix": rows,
            "per_class": per_class,
            "macro_f1": self.macro_f1().map(round),
        })
    }
}

/// Counts for one yes/no decision against labels, at a fixed cut-off.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Binary {
    pub tp: u32,
    pub fp: u32,
    pub tn: u32,
    pub fn_: u32,
    squared_error: f64,
}

impl Binary {
    /// Record a probability of "yes" against a label.
    pub(crate) fn add(&mut self, probability: f64, label: bool, cut_off: f64) {
        let predicted = probability >= cut_off;
        match (predicted, label) {
            (true, true) => self.tp += 1,
            (true, false) => self.fp += 1,
            (false, false) => self.tn += 1,
            (false, true) => self.fn_ += 1,
        }
        let target = if label { 1.0 } else { 0.0 };
        self.squared_error += (probability - target).powi(2);
    }

    pub(crate) fn total(&self) -> u32 {
        self.tp + self.fp + self.tn + self.fn_
    }

    pub(crate) fn false_positive_rate(&self) -> Ratio {
        Ratio::new(self.fp, self.fp + self.tn)
    }

    pub(crate) fn false_negative_rate(&self) -> Ratio {
        Ratio::new(self.fn_, self.fn_ + self.tp)
    }

    /// Mean squared error of the probabilities: lower is better calibrated.
    pub(crate) fn brier(&self) -> Option<f64> {
        (self.total() > 0).then(|| self.squared_error / f64::from(self.total()))
    }

    pub(crate) fn json(&self) -> Value {
        json!({
            "labeled": self.total(),
            "tp": self.tp,
            "fp": self.fp,
            "tn": self.tn,
            "fn": self.fn_,
            "accuracy": Ratio::new(self.tp + self.tn, self.total()).json(),
            "false_positive_rate": self.false_positive_rate().json(),
            "false_negative_rate": self.false_negative_rate().json(),
            "brier": self.brier().map(round),
        })
    }
}

/// One confidence bin of a reliability curve.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Bin {
    pub low: f64,
    pub high: f64,
    pub count: u32,
    pub mean_confidence: Option<f64>,
    pub accuracy: Option<f64>,
}

/// Equal-width reliability bins over `[0, 1]`. The last bin includes 1.0.
pub(crate) fn reliability(points: &[(f64, bool)], bins: usize) -> Vec<Bin> {
    let bins = bins.max(1);
    let mut sums = vec![(0u32, 0.0f64, 0u32); bins];
    for &(confidence, correct) in points {
        let index = ((confidence * bins as f64) as usize).min(bins - 1);
        let slot = &mut sums[index];
        slot.0 += 1;
        slot.1 += confidence;
        slot.2 += u32::from(correct);
    }
    sums.into_iter()
        .enumerate()
        .map(|(index, (count, confidence, correct))| Bin {
            low: index as f64 / bins as f64,
            high: (index + 1) as f64 / bins as f64,
            count,
            mean_confidence: (count > 0).then(|| confidence / f64::from(count)),
            accuracy: (count > 0).then(|| f64::from(correct) / f64::from(count)),
        })
        .collect()
}

/// Expected calibration error: the count-weighted mean gap between
/// confidence and accuracy across bins.
pub(crate) fn expected_calibration_error(bins: &[Bin]) -> Option<f64> {
    let total: u32 = bins.iter().map(|bin| bin.count).sum();
    if total == 0 {
        return None;
    }
    Some(
        bins.iter()
            .filter_map(|bin| {
                let gap = (bin.accuracy? - bin.mean_confidence?).abs();
                Some(f64::from(bin.count) / f64::from(total) * gap)
            })
            .sum(),
    )
}

pub(crate) fn bins_json(bins: &[Bin]) -> Value {
    Value::Array(
        bins.iter()
            .map(|bin| {
                json!({
                    "range": [round(bin.low), round(bin.high)],
                    "count": bin.count,
                    "mean_confidence": bin.mean_confidence.map(round),
                    "accuracy": bin.accuracy.map(round),
                })
            })
            .collect(),
    )
}

/// What one threshold would do over a set of `(confidence, correct)` points,
/// where `total` also counts attempts that produced no confidence at all.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct CoveragePoint {
    pub threshold: f64,
    pub accepted: Ratio,
    pub selective_accuracy: Ratio,
}

impl CoveragePoint {
    pub(crate) fn json(&self) -> Value {
        json!({
            "threshold": self.threshold,
            "coverage": self.accepted.json(),
            "fallback_rate": Ratio::new(
                self.accepted.denominator - self.accepted.numerator,
                self.accepted.denominator,
            )
            .json(),
            "selective_accuracy": self.selective_accuracy.json(),
        })
    }
}

/// Accuracy among accepted predictions against the share accepted, for each
/// threshold. `total` is every attempt, so failures lower coverage.
pub(crate) fn coverage_curve(
    points: &[(f64, bool)],
    total: u32,
    thresholds: &[f64],
) -> Vec<CoveragePoint> {
    thresholds
        .iter()
        .map(|&threshold| {
            let accepted: Vec<&(f64, bool)> = points
                .iter()
                .filter(|(confidence, _)| *confidence >= threshold)
                .collect();
            let correct = accepted.iter().filter(|(_, correct)| *correct).count();
            let accepted_count = u32::try_from(accepted.len()).unwrap_or(u32::MAX);
            CoveragePoint {
                threshold,
                accepted: Ratio::new(accepted_count, total),
                selective_accuracy: Ratio::new(
                    u32::try_from(correct).unwrap_or(u32::MAX),
                    accepted_count,
                ),
            }
        })
        .collect()
}

/// A threshold proposal for review. Never applied automatically.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Candidate {
    /// A grid threshold whose accepted sample is large enough and whose
    /// selective accuracy's Wilson lower bound meets the target.
    Proposed(CoveragePoint),
    /// No threshold met the target with enough accepted samples.
    InsufficientEvidence { reason: &'static str },
}

/// Propose a threshold from `curve`, requiring `min_accepted` accepted
/// samples and a Wilson lower bound of at least `target`.
///
/// The walk starts at the highest threshold that qualifies and moves down
/// only while every step still qualifies, and it settles on a lower threshold
/// only when that threshold actually accepts more samples. A threshold below
/// the observed confidences is never proposed: lowering it would add no
/// evidence, only exposure.
pub(crate) fn candidate_threshold(
    curve: &[CoveragePoint],
    target: f64,
    min_accepted: u32,
) -> Candidate {
    let mut descending: Vec<&CoveragePoint> = curve.iter().collect();
    descending.sort_by(|a, b| b.threshold.total_cmp(&a.threshold));
    let qualifies = |point: &CoveragePoint| {
        point.accepted.numerator >= min_accepted
            && point
                .selective_accuracy
                .wilson()
                .is_some_and(|(low, _)| low >= target)
    };
    if !descending
        .iter()
        .any(|point| point.accepted.numerator >= min_accepted)
    {
        return Candidate::InsufficientEvidence {
            reason: "too_few_accepted_samples",
        };
    }
    let mut points = descending
        .into_iter()
        .skip_while(|point| point.accepted.numerator < min_accepted);
    let Some(first) = points.next().filter(|point| qualifies(point)) else {
        return Candidate::InsufficientEvidence {
            reason: "target_not_met",
        };
    };
    let mut chosen = *first;
    for point in points {
        if !qualifies(point) {
            break;
        }
        if point.accepted.numerator > chosen.accepted.numerator {
            chosen = *point;
        }
    }
    Candidate::Proposed(chosen)
}

/// Nearest-rank percentile of `values`, or `None` when empty.
pub(crate) fn percentile(values: &[u64], percent: f64) -> Option<u64> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let rank = ((percent / 100.0) * sorted.len() as f64).ceil() as usize;
    sorted.get(rank.clamp(1, sorted.len()) - 1).copied()
}

pub(crate) fn latency_json(values_ms: &[u64]) -> Value {
    json!({
        "samples": values_ms.len(),
        "p50_ms": percentile(values_ms, 50.0),
        "p95_ms": percentile(values_ms, 95.0),
        "max_ms": values_ms.iter().max(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn ratios_keep_their_denominators_and_never_divide_by_zero() {
        assert_eq!(Ratio::new(3, 4).value(), Some(0.75));
        assert_eq!(Ratio::new(0, 0).value(), None);
        assert_eq!(Ratio::new(0, 0).wilson(), None);
        let json = Ratio::new(1, 2).json();
        assert_eq!(json["numerator"], 1);
        assert_eq!(json["denominator"], 2);
    }

    #[test]
    fn the_wilson_interval_matches_a_hand_computation() {
        // 8 of 10: centre (0.8 + 0.19208) / 1.38415 = 0.71674, half-width
        // 1.96 * sqrt(0.016 + 0.0096037) / 1.38415 = 0.22656.
        let (low, high) = Ratio::new(8, 10).wilson().unwrap();
        assert!((low - 0.4902).abs() < 1e-3, "{low}");
        assert!((high - 0.9433).abs() < 1e-3, "{high}");
        // A perfect small sample is not reported as certain.
        let (low, high) = Ratio::new(5, 5).wilson().unwrap();
        assert!(low < 0.6 && close(high, 1.0));
    }

    #[test]
    fn per_class_and_macro_scores_match_a_hand_computation() {
        // gold a: a, a, b; gold b: b; gold c: a.
        let mut confusion = Confusion::with_labels(["a", "b", "c"]);
        for (gold, predicted) in [("a", "a"), ("a", "a"), ("a", "b"), ("b", "b"), ("c", "a")] {
            confusion.add(gold, predicted);
        }
        assert_eq!(confusion.total(), 5);
        assert_eq!(confusion.correct(), 3);
        let a = confusion.class("a");
        assert_eq!(
            (a.precision, a.recall),
            (Ratio::new(2, 3), Ratio::new(2, 3))
        );
        assert!(close(a.f1().unwrap(), 2.0 / 3.0));
        let b = confusion.class("b");
        assert_eq!(
            (b.precision, b.recall),
            (Ratio::new(1, 2), Ratio::new(1, 1))
        );
        assert!(close(b.f1().unwrap(), 2.0 / 3.0));
        let c = confusion.class("c");
        assert_eq!(c.precision.value(), None, "c was never predicted");
        assert_eq!(c.recall, Ratio::new(0, 1));
        // (2/3 + 2/3 + 0) / 3
        assert!(close(confusion.macro_f1().unwrap(), 4.0 / 9.0));
        // An unseen label with no gold support does not dilute the mean.
        let mut sparse = Confusion::with_labels(["a", "z"]);
        sparse.add("a", "a");
        assert!(close(sparse.macro_f1().unwrap(), 1.0));
        assert_eq!(Confusion::default().macro_f1(), None);
    }

    #[test]
    fn binary_rates_and_brier_match_a_hand_computation() {
        let mut binary = Binary::default();
        binary.add(0.9, true, 0.5); // tp, error 0.01
        binary.add(0.8, false, 0.5); // fp, error 0.64
        binary.add(0.2, false, 0.5); // tn, error 0.04
        binary.add(0.3, true, 0.5); // fn, error 0.49
        assert_eq!((binary.tp, binary.fp, binary.tn, binary.fn_), (1, 1, 1, 1));
        assert_eq!(binary.false_positive_rate(), Ratio::new(1, 2));
        assert_eq!(binary.false_negative_rate(), Ratio::new(1, 2));
        assert!(close(
            binary.brier().unwrap(),
            (0.01 + 0.64 + 0.04 + 0.49) / 4.0
        ));
        assert_eq!(Binary::default().brier(), None);
    }

    #[test]
    fn reliability_bins_and_ece_match_a_hand_computation() {
        // Bin [0.9, 1.0]: 0.95 right, 0.95 wrong -> accuracy 0.5, gap 0.45.
        // Bin [0.6, 0.7): 0.65 right -> accuracy 1.0, gap 0.35.
        let points = [(0.95, true), (0.95, false), (0.65, true), (1.0, true)];
        let bins = reliability(&points, 10);
        assert_eq!(bins.len(), 10);
        assert_eq!(bins[9].count, 3, "1.0 falls in the last bin");
        assert_eq!(bins[6].count, 1);
        assert_eq!(bins[0].accuracy, None);
        // Last bin: mean confidence 0.9667, accuracy 0.6667, gap 0.3.
        // ECE = 3/4 * 0.3 + 1/4 * 0.35 = 0.3125.
        assert!((expected_calibration_error(&bins).unwrap() - 0.3125).abs() < 1e-9);
        assert_eq!(expected_calibration_error(&reliability(&[], 10)), None);
    }

    #[test]
    fn coverage_counts_failures_and_candidates_need_evidence() {
        // Four confident answers, one wrong; two attempts produced nothing.
        let points = [(0.95, true), (0.92, true), (0.91, false), (0.6, true)];
        let curve = coverage_curve(&points, 6, &[0.5, 0.9, 0.93]);
        assert_eq!(curve[0].accepted, Ratio::new(4, 6));
        assert_eq!(curve[0].selective_accuracy, Ratio::new(3, 4));
        assert_eq!(curve[1].accepted, Ratio::new(3, 6));
        assert_eq!(curve[2].accepted, Ratio::new(1, 6));
        assert_eq!(curve[2].selective_accuracy, Ratio::new(1, 1));
        assert_eq!(
            candidate_threshold(&curve, 0.9, 30),
            Candidate::InsufficientEvidence {
                reason: "too_few_accepted_samples"
            }
        );
        // 40 of 40 correct at 0.95: the Wilson lower bound is about 0.912.
        let many: Vec<(f64, bool)> = (0..40).map(|_| (0.95, true)).collect();
        let curve = coverage_curve(&many, 40, &[0.5, 0.9]);
        let Candidate::Proposed(point) = candidate_threshold(&curve, 0.9, 30) else {
            panic!("expected a proposal");
        };
        assert_eq!(
            point.threshold, 0.9,
            "0.5 accepts nothing more, so it is not evidenced"
        );
        assert_eq!(
            candidate_threshold(&curve, 0.99, 30),
            Candidate::InsufficientEvidence {
                reason: "target_not_met"
            }
        );
        // Forty more correct answers at 0.85 justify going down to 0.8.
        let mut wider = many.clone();
        wider.extend((0..40).map(|_| (0.85, true)));
        let curve = coverage_curve(&wider, 80, &[0.8, 0.9]);
        let Candidate::Proposed(point) = candidate_threshold(&curve, 0.9, 30) else {
            panic!("expected a proposal");
        };
        assert_eq!(point.threshold, 0.8);
        // Forty wrong answers at 0.85 stop the walk at 0.9.
        let mut worse = many.clone();
        worse.extend((0..40).map(|_| (0.85, false)));
        let curve = coverage_curve(&worse, 80, &[0.8, 0.9]);
        let Candidate::Proposed(point) = candidate_threshold(&curve, 0.9, 30) else {
            panic!("expected a proposal");
        };
        assert_eq!(point.threshold, 0.9);
    }

    #[test]
    fn percentiles_use_nearest_rank() {
        let values = [5, 1, 3, 2, 4];
        assert_eq!(percentile(&values, 50.0), Some(3));
        assert_eq!(percentile(&values, 95.0), Some(5));
        assert_eq!(percentile(&values, 0.0), Some(1));
        assert_eq!(percentile(&[], 50.0), None);
    }
}
