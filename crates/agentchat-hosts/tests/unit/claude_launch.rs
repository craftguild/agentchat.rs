use super::*;
use agentchat_core::model::HostKind;

fn context(project: Uuid, root: Uuid, directory: PathBuf, version: &str) -> Context {
    Context {
        context_id: Uuid::new_v4(),
        project_id: project,
        host: HostKind::Claude,
        host_session_id: root.to_string(),
        host_version: version.into(),
        os_profile: "synthetic-platform".into(),
        working_directory: directory,
        incarnation: Uuid::new_v4(),
    }
}

#[test]
fn initial_directory_is_fixed_but_a_same_project_transition_may_use_another_worktree() {
    let project = Uuid::new_v4();
    let root = Uuid::new_v4();
    let directory = PathBuf::from("/synthetic/project");
    let startup = ClaudeStartup::new(root, project, directory.clone(), "2.1.309".into()).unwrap();

    let initial = context(project, root, directory, "2.1.309");
    assert!(startup.matches_observed(&initial));
    assert!(startup.matches_initial(&initial));

    let mut worktree = context(
        project,
        Uuid::new_v4(),
        PathBuf::from("/synthetic/worktree"),
        "2.1.309",
    );
    assert!(startup.matches_observed(&worktree));
    assert!(!startup.matches_initial(&worktree));

    worktree.project_id = Uuid::new_v4();
    assert!(!startup.matches_observed(&worktree));
    worktree.project_id = project;
    worktree.host_version = "2.1.310".into();
    assert!(!startup.matches_observed(&worktree));
}
