//! Projects and their environments: the records under
//! `<org>/apps/projects`, one JSON file per project.

use std::path::PathBuf;

use super::Project;
use super::deploy::Apps;
use crate::error::{Error, Result};
use crate::org::OrgId;

impl Apps {
    fn projects_dir(&self, org: &OrgId) -> PathBuf {
        self.apps_dir(org).join("projects")
    }

    pub fn project_create(
        &self,
        org: &OrgId,
        name: &str,
        description: &str,
        environments: &[String],
    ) -> Result<Project> {
        super::validate_part("project", name)?;
        let envs: Vec<String> = if environments.is_empty() {
            vec![super::DEFAULT_ENVIRONMENT.into()]
        } else {
            environments.to_vec()
        };
        for e in &envs {
            super::validate_part("environment", e)?;
            self.check_env_stack_free(org, name, e)?;
        }
        let _g = self.inner.edit.lock().unwrap();
        let p = self.projects_dir(org).join(format!("{name}.json"));
        if p.exists() {
            return Err(Error::AlreadyExists(format!("project {name}")));
        }
        let mut envs = envs;
        envs.dedup();
        let proj = Project {
            name: name.into(),
            description: description.into(),
            environments: envs,
            created_at: crate::stack::now_secs(),
            compose: Vec::new(),
        };
        self.save_project(org, &proj)?;
        Ok(proj)
    }

    pub fn project_get(&self, org: &OrgId, name: &str) -> Result<Project> {
        super::validate_part("project", name)?;
        let p = self.projects_dir(org).join(format!("{name}.json"));
        match std::fs::read(&p) {
            Ok(b) => Ok(serde_json::from_slice(&b)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Err(Error::NotFound(format!("project {name} in org {org}")))
            }
            Err(e) => Err(e.into()),
        }
    }

    /// The org's projects. Compose stacks that are gone are left out.
    pub fn project_list(&self, org: &OrgId) -> Result<Vec<Project>> {
        let mut out = self.projects_raw(org)?;
        for p in &mut out {
            p.compose.retain(|r| self.stack_live(org, &r.stack));
        }
        Ok(out)
    }

    /// The project records as stored.
    pub(super) fn projects_raw(&self, org: &OrgId) -> Result<Vec<Project>> {
        let mut out = Vec::new();
        let Ok(rd) = std::fs::read_dir(self.projects_dir(org)) else {
            return Ok(out);
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "json") {
                match serde_json::from_slice::<Project>(&std::fs::read(&p)?) {
                    Ok(pr) => out.push(pr),
                    Err(e) => eprintln!("isb serve: skipping {}: {e}", p.display()),
                }
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    /// Delete an empty project (its apps go first).
    pub fn project_delete(&self, org: &OrgId, name: &str) -> Result<()> {
        let _g = self.inner.edit.lock().unwrap();
        self.project_get(org, name)?;
        let apps: Vec<String> = self
            .list(org)?
            .into_iter()
            .filter(|a| a.spec.project == name)
            .map(|a| a.spec.name)
            .collect();
        if !apps.is_empty() {
            return Err(Error::invalid(format!(
                "project {name} still has apps: {}; delete them first",
                apps.join(", ")
            )));
        }
        let stacks = self.live_compose(org, name, None);
        if !stacks.is_empty() {
            return Err(Error::invalid(format!(
                "project {name} still has compose stacks: {}; remove them first (isb stack rm)",
                stacks.join(", ")
            )));
        }
        std::fs::remove_file(self.projects_dir(org).join(format!("{name}.json")))?;
        self.invalidate_owners(org);
        Ok(())
    }

    /// `<project>-<env>` must be a stack name, and not a compose stack's.
    fn check_env_stack_free(&self, org: &OrgId, project: &str, env: &str) -> Result<()> {
        let s = super::stack_name(project, env)?;
        if self.compose_stack_exists(org, &s) {
            return Err(Error::invalid(format!(
                "{project} + {env}: the environment would run as stack {s}, which is a compose stack; pick another name"
            )));
        }
        Ok(())
    }

    pub fn environment_create(&self, org: &OrgId, project: &str, env: &str) -> Result<Project> {
        super::validate_part("environment", env)?;
        self.check_env_stack_free(org, project, env)?;
        let _g = self.inner.edit.lock().unwrap();
        let mut p = self.project_get(org, project)?;
        if p.environments.iter().any(|e| e == env) {
            return Err(Error::AlreadyExists(format!(
                "environment {env} in project {project}"
            )));
        }
        p.environments.push(env.into());
        self.save_project(org, &p)?;
        Ok(p)
    }

    pub fn environment_delete(&self, org: &OrgId, project: &str, env: &str) -> Result<Project> {
        let _g = self.inner.edit.lock().unwrap();
        let mut p = self.project_get(org, project)?;
        if !p.environments.iter().any(|e| e == env) {
            return Err(Error::NotFound(format!(
                "environment {env} in project {project}"
            )));
        }
        let apps: Vec<String> = self
            .list(org)?
            .into_iter()
            .filter(|a| a.spec.project == project && a.spec.environment == env)
            .map(|a| a.spec.name)
            .collect();
        if !apps.is_empty() {
            return Err(Error::invalid(format!(
                "environment {env} still has apps: {}; delete them first",
                apps.join(", ")
            )));
        }
        let stacks = self.live_compose(org, project, Some(env));
        if !stacks.is_empty() {
            return Err(Error::invalid(format!(
                "environment {env} still has compose stacks: {}; remove them first (isb stack rm)",
                stacks.join(", ")
            )));
        }
        p.environments.retain(|e| e != env);
        // Records of its stacks that are gone go with it.
        p.compose.retain(|r| r.environment != env);
        self.save_project(org, &p)?;
        Ok(p)
    }

    pub(super) fn save_project(&self, org: &OrgId, p: &Project) -> Result<()> {
        super::write_atomic(
            &self.projects_dir(org).join(format!("{}.json", p.name)),
            &serde_json::to_vec_pretty(p)?,
        )?;
        self.invalidate_owners(org);
        Ok(())
    }
}
