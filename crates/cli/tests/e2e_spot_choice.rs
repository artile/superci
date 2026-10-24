//! Which spot machine a job gets, end to end: spot pools priced over an on-demand machine left out of the launch (from
//! this computer and from the control plane), and a job's page saying what it got, at what price, the cheaper first
//! choice that had no machine, and what was not asked for at all.
mod common;
use common::signed_in;
use superci_testkit::{serve, PLANE};

/// AWS's spot prices: each (type, price) in each of these zones.
fn prices(rows: &[(&str, f64)], zones: &[&str]) -> String {
    format!("<DescribeSpotPriceHistoryResponse xmlns=\"http://ec2.amazonaws.com/doc/2016-11-15/\"><requestId>1</requestId><spotPriceHistorySet>{}</spotPriceHistorySet><nextToken/></DescribeSpotPriceHistoryResponse>",
        rows.iter().flat_map(|(t, p)| zones.iter().map(move |z| format!("<item><instanceType>{t}</instanceType><productDescription>Linux/UNIX</productDescription><spotPrice>{p:.6}</spotPrice><timestamp>2026-10-15T10:00:00.000Z</timestamp><availabilityZone>{z}</availabilityZone></item>"))).collect::<String>())
}

/// AWS saying a region has these types.
fn offered(types: &[&str], region: &str) -> String {
    format!("<DescribeInstanceTypeOfferingsResponse xmlns=\"http://ec2.amazonaws.com/doc/2016-11-15/\"><requestId>1</requestId><instanceTypeOfferingSet>{}</instanceTypeOfferingSet></DescribeInstanceTypeOfferingsResponse>",
        types.iter().map(|t| format!("<item><instanceType>{t}</instanceType><locationType>region</locationType><location>{region}</location></item>")).collect::<String>())
}

/// AWS's answer to a machine started (as it answered Test).
fn aws_started() -> String {
    include_str!("../fixtures/aws/launch/on-demand.jsonl").lines().filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find(|v| v["asked"].as_str().unwrap_or_default().contains("Action=RunInstances") && v["status"] == 200).map(|v| v["answer"].as_str().unwrap_or_default().to_string()).unwrap()
}

/// The machine types asked of AWS with RunInstances since the `from`th request.
fn run_types(served: &superci_testkit::Served, from: usize) -> Vec<String> {
    served.aws.asked.lock().unwrap().iter().skip(from).filter_map(|a| a["asked"].as_str()).filter(|a| a.contains("Action=RunInstances&"))
        .filter_map(|a| url::form_urlencoded::parse(a.as_bytes()).find(|(k, _)| k == "InstanceType").map(|(_, v)| v.to_string())).collect()
}

/// One machine started from this computer: a type whose spot price is over what the cheapest of the types costs
/// on-demand is not asked for in either of its zones, the other is; with every pool over, none is asked and the
/// launch says why; with no type the region has, nothing is asked.
#[test]
fn spot_pools_priced_over_an_on_demand_machine_are_not_asked_for_and_with_all_of_them_over_none_is() {
    let served = serve(|_| {});
    served.aws.replaying(include_str!("../fixtures/aws/launch/spot.jsonl"), &[]);
    let home = signed_in(&served);
    let types = ["c8a.xlarge", "r8a.xlarge"];
    let started = aws_started();
    let launch = |price: &str, more: &[(&str, u16, String)]| -> (String, Vec<String>) {
        served.aws.fail_none();
        // (No network of the control plane's: its zones are those of the prices.)
        served.aws.fail("DescribeVpcs", 200, "<DescribeVpcsResponse xmlns=\"http://ec2.amazonaws.com/doc/2016-11-15/\"><requestId>1</requestId><vpcSet/></DescribeVpcsResponse>", None);
        served.aws.fail("DescribeInstanceTypeOfferings", 200, &offered(&types, "us-east-1"), None);
        served.aws.fail("DescribeSpotPriceHistory", 200, price, None);
        for (action, status, answer) in more { served.aws.fail(action, *status, answer, None) }
        let from = served.aws.asked.lock().unwrap().len();
        let (_, said) = common::json(&home, &["debug", "launch", "us-east-1", "spot", &types.join(",")]);
        served.aws.fail_none();
        (said["answer"].as_str().unwrap_or_default().to_string(), run_types(&served, from))
    };
    // A c8a.xlarge costs about $0.205 an hour on-demand: r8a.xlarge's spot price is over that in both zones.
    let (said, asked) = launch(&prices(&[("c8a.xlarge", 0.08), ("r8a.xlarge", 0.25)], &["us-east-1a", "us-east-1b"]), &[("RunInstances", 200, started)]);
    assert!(said.starts_with("started ") && said.contains("c8a.xlarge"), "{said}");
    assert!(!asked.is_empty() && asked.iter().all(|t| t == "c8a.xlarge"), "{asked:?}");
    // Every pool over it: not one asked.
    let (said, asked) = launch(&prices(&[("c8a.xlarge", 0.5), ("r8a.xlarge", 0.6)], &["us-east-1a", "us-east-1b"]), &[]);
    assert!(said.starts_with("not started: InsufficientInstanceCapacity: every spot pool cost more than an on-demand machine ($0.205 an hour)"), "{said}");
    assert!(asked.is_empty(), "{asked:?}");
    // No type the region has: no pool at all, which is not every pool priced over.
    served.aws.fail("DescribeVpcs", 200, "<DescribeVpcsResponse xmlns=\"http://ec2.amazonaws.com/doc/2016-11-15/\"><requestId>1</requestId><vpcSet/></DescribeVpcsResponse>", None);
    served.aws.fail("DescribeInstanceTypeOfferings", 200, &offered(&[], "us-east-1"), None);
    let (_, said) = common::json(&home, &["debug", "launch", "us-east-1", "spot", "x9z.large"]);
    served.aws.fail_none();
    let said = said["answer"].as_str().unwrap_or_default();
    assert!(said.starts_with("not started: no instance type given"), "{said}");
    let _ = std::fs::remove_dir_all(&home);
}

/// A job of the control plane whose every spot pool, in each of its two regions, costs more than an on-demand
/// machine: no spot machine asked for, the types kept with the job once each, and the job on an on-demand machine as
/// its order goes on.
#[test]
fn a_job_whose_spot_pools_all_cost_more_than_on_demand_in_each_region_keeps_those_types_once_and_runs_on_demand() {
    let served = serve(|c| {
        (c.aws_own, c.aws_own_creds) = (Some(("111111111111".into(), "us-east-1".into())), Some(superci_core::aws::Credentials { access_key_id: "ASIAX".into(), secret_access_key: "secret".into(), session_token: Some("tok".into()), expires_at_ms: u64::MAX }));
        c.aws_regions = vec!["us-east-1".into(), "us-east-2".into()];
    });
    served.connect_github();
    *served.public.lock().unwrap() = Some("https://plane.acme.test".into());
    served.aws.put(&format!("/superci/{PLANE}/ROUTING"), r#"{"order":[{"cloud":"aws","max_jobs":100,"regions":["us-east-1","us-east-2"]},{"cloud":"aws-on-demand"},{"cloud":"cloudflare","off":true}]}"#);
    served.aws.replaying(include_str!("../fixtures/aws/launch/spot.jsonl"), &[]);
    let types = ["c8a.xlarge", "m8a.xlarge", "c7a.xlarge", "m7a.xlarge", "c8i.xlarge", "m8i.xlarge", "c7i.xlarge", "m7i.xlarge"];
    served.aws.fail("DescribeInstanceTypeOfferings", 200, &offered(&types, "us-east-1"), None);
    // (The control plane's network is in us-east-1, its subnet in us-east-1d; us-east-2 has AWS's default network.)
    served.aws.fail("DescribeVpcs|ec2.us-east-2", 200, "<DescribeVpcsResponse xmlns=\"http://ec2.amazonaws.com/doc/2016-11-15/\"><requestId>1</requestId><vpcSet/></DescribeVpcsResponse>", None);
    let rows: Vec<(&str, f64)> = types.iter().map(|t| (*t, 0.9)).collect();
    served.aws.fail("DescribeSpotPriceHistory", 200, &prices(&rows, &["us-east-1d", "us-east-1a"]), None);
    served.aws.fail("RunInstances|MarketType=spot", 400, &superci_testkit::ec2_error("InvalidParameterValue", "Not to be asked here."), None);
    served.aws.fail("RunInstances", 200, &aws_started(), None);
    served.store.0.lock().unwrap().insert("aws:no-fleet".into(), served.now_ms().to_string());
    let from = served.aws.asked.lock().unwrap().len();
    let (id, q) = served.github.queue("priced", "superci", served.now_ms());
    assert!(served.webhook(q) < 300);
    let j = served.store.under(&format!("job:{id}"))[0].1.clone();
    assert!(j["state"] == "launched" && j["on_demand"] == true, "{j}");
    // (Both regions tried for spot, each with no pool to ask.)
    let refused = j["spot_refused"].as_str().unwrap_or_default();
    assert!(refused.contains("us-east-1: InsufficientInstanceCapacity") && refused.contains("us-east-2: InsufficientInstanceCapacity"), "{j}");
    let out: Vec<&str> = j["priced_out"].as_array().unwrap().iter().filter_map(|k| k.as_str()).collect();
    let mut want = types.to_vec();
    want.sort();
    let mut got = out.clone();
    got.sort();
    assert_eq!(got, want, "each type once, though both regions priced it out: {j}");
    let spot_asked = served.aws.asked.lock().unwrap().iter().skip(from).filter_map(|a| a["asked"].as_str()).filter(|a| a.contains("Action=RunInstances&") && a.contains("MarketType=spot")).count();
    assert_eq!(spot_asked, 0, "no spot machine asked for: {j}");
    assert!(!run_types(&served, from).is_empty(), "an on-demand machine asked for");
    served.aws.fail_none();
}

/// A spot job's page, from what AWS answered each ask: the price of the machine it got and the cheaper first choice
/// that had no machine (asks of on-demand machines, of many pools at once, and with no price, are not that choice);
/// a first choice priced over what it got is not said; the types priced over an on-demand machine are.
#[test]
fn a_spot_jobs_page_says_its_price_its_cheaper_first_choice_and_the_types_priced_over_on_demand() {
    let served = serve(|_| {});
    served.connect_github();
    let home = signed_in(&served);
    let (id, queued) = served.github.queue("tests", "superci", served.now_ms());
    assert!(served.webhook(queued) < 300);
    let job: serde_json::Value = served.store.under(&format!("job:{id}"))[0].1.clone();
    // (Read anew each time: no view kept between the two records.)
    let (mut child, link) = common::dashboard_with(&home, &[("SUPERCI_VIEWS_S", "0")]);
    let market = |asks: serde_json::Value, priced_out: serde_json::Value| -> String {
        let mut j = job.clone();
        for (k, v) in [("cloud", "aws".into()), ("machine_id", "i-0a1b2c3d4e5f60718".into()), ("machine_type", "c7a.xlarge".into()), ("cpu", 4.into()), ("region", "us-east-1".into()), ("zone", "us-east-1d".into()),
            ("asks", asks), ("priced_out", priced_out), ("rev", (job["rev"].as_u64().unwrap_or(0) + 1).into())] { j[k] = v }
        served.store.0.lock().unwrap().insert(format!("job:{id}"), j.to_string());
        let page = common::words(&common::part(&link, &format!("/?p=job&id={id}&fragment=1")).1);
        page.split("Spot or on-demand ").nth(1).and_then(|s| s.split(" Region ").next()).unwrap_or_else(|| panic!("no row: {page}")).to_string()
    };
    // Read from the end for what it got (an on-demand ask and a spot one with no machine after it), and from the
    // start for its first choice (an on-demand ask, one of many pools, one with no price before it).
    let asks = serde_json::json!([
        { "at": 1, "ms": 300, "kind": "m8a.xlarge", "spot": false, "said": "InsufficientInstanceCapacity" },
        { "at": 1, "ms": 300, "kind": "r8a.xlarge", "spot": true, "said": "InsufficientInstanceCapacity", "pools": 14, "usd": 0.02 },
        { "at": 1, "ms": 300, "kind": "m7a.xlarge", "spot": true, "said": "InsufficientInstanceCapacity", "zone": "us-east-1b" },
        { "at": 1, "ms": 300, "kind": "c8a.xlarge", "spot": true, "said": "InsufficientInstanceCapacity", "zone": "us-east-1a", "usd": 0.05 },
        { "at": 1, "ms": 300, "kind": "c7a.xlarge", "spot": true, "said": "started", "zone": "us-east-1d", "usd": 0.08 },
        { "at": 1, "ms": 300, "kind": "c7a.xlarge", "spot": true, "said": "InsufficientInstanceCapacity", "zone": "us-east-1c", "usd": 0.07 },
        { "at": 1, "ms": 300, "kind": "m8a.xlarge", "spot": false, "said": "InsufficientInstanceCapacity" },
    ]);
    let said = market(asks, serde_json::json!([]));
    assert_eq!(said, "Spot · $0.0800 an hour · its first choice, c8a.xlarge in us-east-1a at $0.0500, had no machine");
    // Its first choice cost more than the machine it got: not a cheaper choice missed. (The machine of an earlier try
    // comes before it, and is neither.) A type priced over on-demand.
    let asks = serde_json::json!([
        { "at": 1, "ms": 300, "kind": "c8a.xlarge", "spot": true, "said": "started", "zone": "us-east-1a", "usd": 0.06 },
        { "at": 1, "ms": 300, "kind": "m8a.xlarge", "spot": true, "said": "InsufficientInstanceCapacity", "zone": "us-east-1a", "usd": 0.12 },
        { "at": 1, "ms": 300, "kind": "c7a.xlarge", "spot": true, "said": "started", "zone": "us-east-1d", "usd": 0.08 },
    ]);
    let said = market(asks, serde_json::json!(["r8a.xlarge", "m7i.xlarge"]));
    assert_eq!(said, "Spot · $0.0800 an hour · not asked, priced over an on-demand machine: r8a.xlarge, m7i.xlarge");
    served.store.0.lock().unwrap().insert(format!("job:{id}"), job.to_string());
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}
