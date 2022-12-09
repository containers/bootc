//! This module holds implementations of some basic statistical properties, such as mean and standard deviation.

pub(crate) fn mean(data: &[u64]) -> Option<f64> {
    if data.is_empty() {
        None
    } else {
        Some(data.iter().sum::<u64>() as f64 / data.len() as f64)
    }
}

pub(crate) fn std_deviation(data: &[u64]) -> Option<f64> {
    match (mean(data), data.len()) {
        (Some(data_mean), count) if count > 0 => {
            let variance = data
                .iter()
                .map(|value| {
                    let diff = data_mean - (*value as f64);
                    diff * diff
                })
                .sum::<f64>()
                / count as f64;
            Some(variance.sqrt())
        }
        _ => None,
    }
}

//Assumed sorted
pub(crate) fn median_absolute_deviation(data: &mut [u64]) -> Option<(f64, f64)> {
    if data.is_empty() {
        None
    } else {
        //Sort data
        //data.sort_by(|a, b| a.partial_cmp(b).unwrap());

        //Find median of data
        let median_data: f64 = match data.len() % 2 {
            1 => data[data.len() / 2] as f64,
            _ => 0.5 * (data[data.len() / 2 - 1] + data[data.len() / 2]) as f64,
        };

        //Absolute deviations
        let mut absolute_deviations = Vec::new();
        for size in data {
            absolute_deviations.push(f64::abs(*size as f64 - median_data))
        }

        absolute_deviations.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let l = absolute_deviations.len();
        let mad: f64 = match l % 2 {
            1 => absolute_deviations[l / 2],
            _ => 0.5 * (absolute_deviations[l / 2 - 1] + absolute_deviations[l / 2]),
        };

        Some((median_data, mad))
    }
}

#[test]
fn test_mean() {
    assert_eq!(mean(&[]), None);
    for v in [0u64, 1, 5, 100] {
        assert_eq!(mean(&[v]), Some(v as f64));
    }
    assert_eq!(mean(&[0, 1]), Some(0.5));
    assert_eq!(mean(&[0, 5, 100]), Some(35.0));
    assert_eq!(mean(&[7, 4, 30, 14]), Some(13.75));
}

#[test]
fn test_std_deviation() {
    assert_eq!(std_deviation(&[]), None);
    for v in [0u64, 1, 5, 100] {
        assert_eq!(std_deviation(&[v]), Some(0 as f64));
    }
    assert_eq!(std_deviation(&[1, 4]), Some(1.5));
    assert_eq!(std_deviation(&[2, 2, 2, 2]), Some(0.0));
    assert_eq!(
        std_deviation(&[1, 20, 300, 4000, 50000, 600000, 7000000, 80000000]),
        Some(26193874.56387471)
    );
}

#[test]
fn test_median_absolute_deviation() {
    //Assumes sorted
    assert_eq!(median_absolute_deviation(&mut []), None);
    for v in [0u64, 1, 5, 100] {
        assert_eq!(median_absolute_deviation(&mut [v]), Some((v as f64, 0.0)));
    }
    assert_eq!(median_absolute_deviation(&mut [1, 4]), Some((2.5, 1.5)));
    assert_eq!(
        median_absolute_deviation(&mut [2, 2, 2, 2]),
        Some((2.0, 0.0))
    );
    assert_eq!(
        median_absolute_deviation(&mut [
            1, 2, 3, 3, 4, 4, 4, 5, 5, 6, 6, 6, 7, 7, 7, 8, 9, 12, 52, 90
        ]),
        Some((6.0, 2.0))
    );

    //if more than half of the data has the same value, MAD = 0, thus any
    //value different from the residual median is classified as an outlier
    assert_eq!(
        median_absolute_deviation(&mut [0, 1, 1, 1, 1, 1, 1, 1, 0]),
        Some((1.0, 0.0))
    );
}
