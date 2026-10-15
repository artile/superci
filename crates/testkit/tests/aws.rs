//! The stand-in for AWS answers as AWS does: what a real account's SSM answered, asked of it again in the same order
//! from nothing (`crates/cli/fixtures/aws/settings/ssm.jsonl`), is answered the same, but for when each parameter
//! was written and the order of a page.
use serde_json::Value;
use superci_testkit::Aws;

#[test]
fn ssm_is_answered_as_a_real_account_answered() {
    let aws = Aws::default();
    let when = |mut v: Value| {
        for p in v["Parameters"].as_array_mut().into_iter().flatten() { p["LastModifiedDate"] = Value::Null }
        if let Some(p) = v.get_mut("Parameter") { p["LastModifiedDate"] = Value::Null }
        v
    };
    for (n, line) in include_str!("../../cli/fixtures/aws/settings/ssm.jsonl").lines().enumerate() {
        let real: Value = serde_json::from_str(line).unwrap();
        let (status, answer) = aws.handle("POST", real["url"].as_str().unwrap(), real["target"].as_str().unwrap(), real["asked"].as_str().unwrap());
        let mut want: Value = serde_json::from_str(real["answer"].as_str().unwrap()).unwrap();
        let mut got: Value = serde_json::from_str(&answer).unwrap();
        // A page of some: SSM gives them in no set order (the same two came as NAME, LABELS and as LABELS, NAME), so which
        // one is not compared, only what it says of it. Its token is AWS's own; this one names the next page its way.
        if want["NextToken"].is_string() {
            want["NextToken"] = "next-1".into();
            for v in [&mut want, &mut got] { for p in v["Parameters"].as_array_mut().into_iter().flatten() { for k in ["ARN", "Name", "Version"] { p[k] = Value::Null } } }
        }
        let answer = got.to_string();
        assert_eq!((status, when(serde_json::from_str(&answer).unwrap())), (real["status"].as_u64().unwrap() as u16, when(want)), "exchange {} ({})", n + 1, real["target"]);
    }
    assert!(aws.unanswered.lock().unwrap().is_empty());
}
