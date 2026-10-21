use super::*;

fn scratch() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "lightagent-workers-{}",
        crate::ids::RunId::new().as_str()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|name| (*name).to_owned()).collect()
}

#[test]
fn every_role_has_a_versioned_skill_and_a_bounded_ceiling() {
    for role in WorkerRole::ALL {
        let worker = builtin(role);
        assert_eq!(worker.role, role);
        assert_eq!(WorkerRole::parse(role.as_str()), Some(role));
        let skill = worker.skill();
        assert_eq!(skill.name, format!("worker-{}", role.as_str()));
        assert_eq!(skill.version.as_deref(), Some("1"));
        assert!(skill.body.len() > 500, "{role:?} skill is not substantive");
        let persona = worker.persona();
        assert!(persona.contains(&skill.body));
        assert!(persona.contains("untrusted data"));
        // No built-in may ever be offered delegation, a local terminal or
        // remote execution.
        for tool in [
            "agent.delegate",
            "terminal.run",
            "open_terminal.run",
            "jev.classify_task",
        ] {
            assert!(!worker.permits(tool), "{role:?} permits {tool}");
        }
        let limits = worker.limits();
        assert!(limits.max_turns <= 8 && limits.max_tool_calls <= 16);
        assert!(limits.wall_clock_secs.is_some());
    }
}

#[test]
fn the_read_only_roles_cannot_write() {
    for role in [WorkerRole::Researcher, WorkerRole::Reviewer] {
        let worker = builtin(role);
        assert!(worker.read_only);
        assert!(!worker.permits("fs.write"));
    }
    assert!(builtin(WorkerRole::Coder).permits("fs.write"));
}

#[test]
fn a_namespace_ceiling_matches_only_its_namespace() {
    let tester = builtin(WorkerRole::Tester);
    assert!(tester.permits("actions.dispatch"));
    assert!(!tester.permits("actions"));
    assert!(!tester.permits("actionsx.dispatch"));
    assert!(!tester.permits("web.fetch"));
}

#[test]
fn readiness_names_the_missing_capability() {
    let researcher = builtin(WorkerRole::Researcher);
    assert_eq!(
        researcher.assess(&names(&["web.search", "web.fetch"])),
        (Readiness::Ready, Vec::new())
    );
    assert_eq!(
        researcher.assess(&names(&["rag.search"])),
        (Readiness::Degraded, vec!["web_search", "page_reading"])
    );
    assert_eq!(
        researcher.assess(&names(&["datetime.now"])).0,
        Readiness::Unavailable
    );
    let tester = builtin(WorkerRole::Tester);
    assert_eq!(
        tester.assess(&names(&["fs.read"])),
        (
            Readiness::Unavailable,
            vec!["workflow_dispatch", "workflow_inspection"]
        )
    );
}

#[test]
fn assignments_are_one_per_role_and_survive_a_reload() {
    let dir = scratch();
    let owner = ProfileId::new("lead").unwrap();
    let store = WorkerAssignments::new(owner.clone(), &dir);
    assert!(store.list().unwrap().is_empty());
    let first = store
        .assign(
            WorkerRole::Researcher,
            AssignmentTarget::Profile {
                profile: ProfileId::new("deep-research").unwrap(),
            },
            10,
        )
        .unwrap();
    assert!(first.id.starts_with("wa-") && first.id.len() == 19);
    let second = store
        .assign(WorkerRole::Researcher, AssignmentTarget::Builtin, 11)
        .unwrap();
    assert_ne!(first.id, second.id);
    let reloaded = WorkerAssignments::new(owner, &dir);
    assert_eq!(reloaded.list().unwrap(), vec![second.clone()]);
    assert_eq!(reloaded.get(WorkerRole::Researcher).unwrap(), Some(second));
    assert_eq!(reloaded.get(WorkerRole::Coder).unwrap(), None);
    assert!(reloaded.revoke(WorkerRole::Researcher).unwrap());
    assert!(!reloaded.revoke(WorkerRole::Researcher).unwrap());
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn a_profile_cannot_assign_itself_and_a_corrupt_file_is_an_error() {
    let dir = scratch();
    let owner = ProfileId::new("lead").unwrap();
    let store = WorkerAssignments::new(owner.clone(), &dir);
    assert_eq!(
        store.assign(
            WorkerRole::Coder,
            AssignmentTarget::Profile { profile: owner },
            1
        ),
        Err(AssignmentError::SelfAssignment)
    );
    std::fs::write(dir.join(ASSIGNMENTS_FILE), b"not json").unwrap();
    assert_eq!(store.list(), Err(AssignmentError::Unreadable));
    std::fs::write(
        dir.join(ASSIGNMENTS_FILE),
        br#"{"schema":99,"assignments":[]}"#,
    )
    .unwrap();
    assert_eq!(store.list(), Err(AssignmentError::Newer));
    std::fs::remove_dir_all(dir).ok();
}
