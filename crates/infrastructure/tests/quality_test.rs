use infrastructure::transcoder::quality::{parse_quality_tiers, QualityLevel};

#[test]
fn parse_single_tier() {
    assert_eq!(parse_quality_tiers("low"), vec![QualityLevel::Low]);
}

#[test]
fn parse_full_ladder() {
    assert_eq!(
        parse_quality_tiers("low,medium,high"),
        vec![QualityLevel::Low, QualityLevel::Medium, QualityLevel::High],
    );
}

#[test]
fn parse_preserves_order() {
    // The master playlist lists variants in the order returned here,
    // and the viewer's player picks the first variant by default.
    // Preserving input order lets operators choose the "default pick"
    // by putting the cheapest tier first.
    assert_eq!(
        parse_quality_tiers("high,low"),
        vec![QualityLevel::High, QualityLevel::Low],
    );
}

#[test]
fn parse_trims_whitespace_and_is_case_insensitive() {
    assert_eq!(
        parse_quality_tiers(" Low , MEDIUM "),
        vec![QualityLevel::Low, QualityLevel::Medium],
    );
}

#[test]
fn parse_dedupes() {
    assert_eq!(
        parse_quality_tiers("low,low,medium,low"),
        vec![QualityLevel::Low, QualityLevel::Medium],
    );
}

#[test]
fn parse_empty_falls_back_to_default() {
    assert_eq!(parse_quality_tiers(""), QualityLevel::all().to_vec());
}

#[test]
fn parse_all_unknown_falls_back_to_default() {
    // A misconfigured env var should degrade gracefully to the full
    // ladder rather than kill the worker at startup.
    assert_eq!(
        parse_quality_tiers("ultra,extreme"),
        QualityLevel::all().to_vec()
    );
}

#[test]
fn parse_mixed_known_and_unknown_keeps_known() {
    assert_eq!(
        parse_quality_tiers("low,garbage,high"),
        vec![QualityLevel::Low, QualityLevel::High],
    );
}
