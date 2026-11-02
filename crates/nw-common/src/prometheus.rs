// SPDX-License-Identifier: AGPL-3.0-or-later
//! Prometheus text from the metrics listeners (M5 spec section 11.3, admin tools spec section
//! 5): counters, gauges and histogram buckets, summed over several scrapes. Only plain sample
//! lines are read: `name value` or `name{label="value",...} value`; comments and other lines are
//! skipped.

/// Two bucket bounds closer than this are the same bound.
const BOUND_TOLERANCE: f64 = 1e-9;
/// Suffix of histogram bucket series.
const BUCKET_SUFFIX: &str = "_bucket";
/// The bound label of a bucket.
const BOUND_LABEL: &str = "le";
/// Suffix of the sample count of a histogram.
const COUNT_SUFFIX: &str = "_count";

#[derive(Debug, Clone, PartialEq)]
struct Sample {
    name: String,
    labels: Vec<(String, String)>,
    value: f64,
}

/// The samples of one or more scrapes.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Scrape {
    samples: Vec<Sample>,
}

impl Scrape {
    /// The samples of a Prometheus text.
    pub fn parse(text: &str) -> Self {
        Self {
            samples: text.lines().filter_map(sample).collect(),
        }
    }

    /// No sample was read: the text was not Prometheus text.
    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    /// Adds the samples of `other`.
    pub fn add(&mut self, other: &Self) {
        self.samples.extend(other.samples.iter().cloned());
    }

    /// The sum of the series `name`, only those with `label` when given.
    pub fn total(&self, name: &str, label: Option<(&str, &str)>) -> f64 {
        self.samples
            .iter()
            .filter(|sample| sample.name == name)
            .filter(|sample| {
                label.is_none_or(|(key, value)| {
                    sample.labels.iter().any(|(sample_key, sample_value)| {
                        sample_key == key && sample_value == value
                    })
                })
            })
            .map(|sample| sample.value)
            .sum()
    }

    /// The sum of the bucket of `histogram` with the bound `bound`.
    pub fn bucket(&self, histogram: &str, bound: f64) -> f64 {
        let name = format!("{histogram}{BUCKET_SUFFIX}");
        self.samples
            .iter()
            .filter(|sample| sample.name == name)
            .filter(|sample| {
                sample.labels.iter().any(|(key, value)| {
                    key == BOUND_LABEL
                        && value.parse::<f64>().is_ok_and(|parsed| {
                            parsed == bound || (parsed - bound).abs() < BOUND_TOLERANCE
                        })
                })
            })
            .map(|sample| sample.value)
            .sum()
    }
}

impl Scrape {
    /// The upper bound of the first bucket of `histogram` whose count reaches the share `q` of
    /// its `_count`, over every scrape added; `None` when the count is 0. The last bucket's bound
    /// is infinite.
    pub fn quantile(&self, histogram: &str, q: f64) -> Option<f64> {
        let count = self.total(&format!("{histogram}{COUNT_SUFFIX}"), None);
        if count <= 0.0 {
            return None;
        }
        let name = format!("{histogram}{BUCKET_SUFFIX}");
        let mut bounds: Vec<f64> = self
            .samples
            .iter()
            .filter(|sample| sample.name == name)
            .filter_map(bound_of)
            .collect();
        bounds.sort_by(f64::total_cmp);
        bounds.dedup_by(|a, b| (*a - *b).abs() < BOUND_TOLERANCE || a == b);
        let wanted = q * count;
        bounds
            .into_iter()
            .find(|bound| self.bucket(histogram, *bound) >= wanted)
    }

    /// The change from `earlier` to `self`, series by series (for rates between two scrapes). A
    /// value that went down is a restarted process: its new value is the change. A series
    /// missing from `earlier` counts from 0.
    pub fn delta(&self, earlier: &Self) -> Self {
        let samples = self
            .samples
            .iter()
            .map(|sample| {
                let before = earlier
                    .samples
                    .iter()
                    .find(|old| old.name == sample.name && old.labels == sample.labels)
                    .map_or(0.0, |old| old.value);
                let value = if sample.value >= before {
                    sample.value - before
                } else {
                    sample.value
                };
                Sample {
                    value,
                    ..sample.clone()
                }
            })
            .collect();
        Self { samples }
    }
}

/// The bound of a bucket sample (`+Inf` is infinite).
fn bound_of(sample: &Sample) -> Option<f64> {
    sample
        .labels
        .iter()
        .find(|(key, _)| key == BOUND_LABEL)
        .and_then(|(_, value)| value.parse().ok())
}

/// One sample line, or `None` for any other line.
fn sample(line: &str) -> Option<Sample> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let (series, value) = line.rsplit_once(' ')?;
    let value = value.parse().ok()?;
    let (name, labels) = match series.split_once('{') {
        Some((name, rest)) => (name, labels(rest.strip_suffix('}')?)?),
        None => (series, Vec::new()),
    };
    Some(Sample {
        name: name.to_owned(),
        labels,
        value,
    })
}

/// `key="value"` pairs, comma separated.
fn labels(text: &str) -> Option<Vec<(String, String)>> {
    text.split(',')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (key, value) = pair.split_once('=')?;
            let value = value.strip_prefix('"')?.strip_suffix('"')?;
            Some((key.to_owned(), value.to_owned()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT: &str = "# TYPE sim_handoffs_total counter
sim_handoffs_total{result=\"moved\"} 12
sim_handoffs_total{result=\"timeout\"} 1
# TYPE sim_handoff_seconds histogram
sim_handoff_seconds_bucket{le=\"0.02\"} 10
sim_handoff_seconds_bucket{le=\"0.033\"} 12
sim_handoff_seconds_bucket{le=\"+Inf\"} 13
sim_handoff_seconds_sum 0.2
sim_handoff_seconds_count 13
sim_players 4
";

    #[test]
    fn a_text_without_samples_is_empty() {
        assert!(Scrape::parse("<html>not metrics</html>\n# HELP x y\n").is_empty());
        assert!(!Scrape::parse(TEXT).is_empty());
    }

    #[test]
    fn histogram_buckets_parse() {
        let scrape = Scrape::parse(TEXT);
        assert_eq!(
            scrape.total("sim_handoffs_total", Some(("result", "moved"))),
            12.0
        );
        assert_eq!(scrape.total("sim_handoffs_total", None), 13.0);
        assert_eq!(scrape.total("sim_players", None), 4.0);
        assert_eq!(scrape.total("missing", None), 0.0);
        assert_eq!(scrape.bucket("sim_handoff_seconds", 0.033), 12.0);
        assert_eq!(scrape.total("sim_handoff_seconds_count", None), 13.0);
    }

    #[test]
    fn quantile_gives_the_bucket_bound() {
        let scrape = Scrape::parse(TEXT);
        assert_eq!(scrape.quantile("sim_handoff_seconds", 0.5), Some(0.02));
        assert_eq!(scrape.quantile("sim_handoff_seconds", 0.9), Some(0.033));
        assert_eq!(
            scrape.quantile("sim_handoff_seconds", 0.99),
            Some(f64::INFINITY)
        );
    }

    #[test]
    fn quantile_of_an_empty_histogram_is_none() {
        let empty = Scrape::parse("h_bucket{le=\"0.1\"} 0\nh_count 0\n");
        assert_eq!(empty.quantile("h", 0.5), None);
        assert_eq!(Scrape::default().quantile("h", 0.5), None);
    }

    #[test]
    fn delta_of_a_restarted_counter_is_the_new_value() {
        let earlier = Scrape::parse("c{a=\"1\"} 10\nd 5\nh_bucket{le=\"0.1\"} 4\n");
        let later = Scrape::parse("c{a=\"1\"} 3\nd 9\nh_bucket{le=\"0.1\"} 7\nnew 2\n");
        let delta = later.delta(&earlier);
        assert_eq!(
            delta.total("c", None),
            3.0,
            "a counter that went down restarted"
        );
        assert_eq!(delta.total("d", None), 4.0);
        assert_eq!(delta.bucket("h", 0.1), 3.0);
        assert_eq!(delta.total("new", None), 2.0);
    }

    #[test]
    fn scrapes_add_up_and_lines_that_are_not_samples_are_skipped() {
        let mut sum = Scrape::parse(TEXT);
        sum.add(&Scrape::parse(
            "sim_players 6\nnot a sample line\nsim_x{a=\"1\" 3\n",
        ));
        assert_eq!(sum.total("sim_players", None), 10.0);
        assert_eq!(sum.total("sim_x", None), 0.0);
    }
}
