use super::{Accounts, Credential, Provider};
use crate::term;
use crate::spawn::Plan;
use crate::Ctx;

pub struct Fake;
pub static PROVIDER: Fake = Fake;

impl Provider for Fake {
    fn vendor(&self) -> Option<&'static str> {
        Some("fake")
    }
    fn start_cmd(&self, _ctx: &Ctx, _p: &Plan, pane: &str) -> Vec<String> {
        term::run_argv(pane, &["fake-cli", "--go"])
    }
    fn env(&self) -> &'static [&'static str] {
        &["FAKE_QUIET=1"]
    }
    fn credential(&self) -> Credential {
        Credential::Account(Accounts {
            provider: "fake",
            config_env: "FAKE_HOME",
            token_env: Some("FAKE_TOKEN"),
        })
    }
    fn login(&self) -> Option<&'static str> {
        Some("fake login")
    }
}

#[cfg(test)]
mod tests {
    use crate::config::Config;
    use crate::spawn::Plan;
    use crate::Ctx;
    use std::path::PathBuf;

    const FLEET: &str = "[fleet.app]\nrepo = \"o/app\"\nlocal = \"/x/app\"\nintegration = \"staging\"\n\
                         [fleet.app.role.impl]\nharness = \"{h}\"\n[accounts]\nA = \"A\"\n";

    fn config(harness: &str) -> anyhow::Result<Config> {
        Config::parse(&FLEET.replace("{h}", harness))
    }

    fn plan() -> Plan {
        let (s, wt) = (String::new, PathBuf::from("/w/app/issue-1"));
        Plan {
            agent: "app-impl-1".into(),
            role: "impl".into(),
            stage: "impl".into(),
            family: "impl",
            harness: "fake".into(),
            profile: "fleet-impl".into(),
            model: None,
            effort: None,
            branch: "issue-1".into(),
            base: "staging".into(),
            worktree: wt.clone(),
            brief: wt.join(".brief.md"),
            kick: "go".into(),
            lane: "app:1:impl".into(),
            account: None,
            env: std::collections::BTreeMap::default(),
            port: 41000,
            mcp: serde_json::Map::default(),
            mcp_file: wt.join(".mcp.lane.json"),
            head: s(),
            title: s(),
            resting: None,
            session: None,
            lane_key: None,
            token: None,
        }
    }

    fn ctx() -> Ctx {
        let d = PathBuf::from("/nowhere");
        Ctx {
            config: d.join("fleet.toml"),
            state: d.join("events.jsonl"),
            worktrees: d.join("w"),
            homes: std::collections::BTreeMap::default(),
            exe: "drover".into(),
            now: 0,
            home: d,
            hints: vec![],
        }
    }

    #[test]
    fn a_role_may_name_the_registered_provider_and_no_unregistered_one() {
        assert!(super::super::lane_names().contains(&"fake"));
        assert_eq!(config("fake").unwrap().fleet["app"].role["impl"].harness, "fake");
        let err = config("acme").err().unwrap().to_string();
        assert!(err.contains("harness must be one of") && err.contains("\"fake\""), "{err}");
    }

    #[test]
    fn setup_offers_its_login_with_its_vendor() {
        let all = crate::setup::all(&config("fake").unwrap());
        assert!(all.contains(&("fake".into(), "fake login".into())), "{all:?}");
        let v = crate::setup::vendors(&config("fake").unwrap());
        assert!(v.contains(&("fake".into(), "fake".into())) && !v.iter().any(|x| x.0 == "gh"), "{v:?}");
    }

    #[test]
    fn an_account_signs_in_through_its_config_folder_env() {
        let cfg = config("fake").unwrap();
        let env = crate::access::account_env(&cfg, &crate::runner::Real, "fake", "A").unwrap();
        let folder = cfg.accounts.folder("A").display().to_string();
        assert_eq!(env, [("FAKE_HOME".to_string(), folder)].into());
    }

    #[test]
    fn a_lane_launches_with_its_command_and_env() {
        let p = plan();
        assert_eq!(p.start_cmd(&ctx(), "w1:p1"), crate::term::run_argv("w1:p1", &["fake-cli", "--go"]));
        let shell = p.lane_cmd("w1:p1").join(" ");
        assert!(shell.contains("FAKE_QUIET=1") && shell.contains("&& unset FAKE_TOKEN"), "{shell}");
    }
}
