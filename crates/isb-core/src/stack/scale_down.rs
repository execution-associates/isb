//! Scaling a service down (or to 0, a Stop): the instances past the
//! replica count are retired, highest slots first.

use super::*;

impl Worker {
    /// Retire the instances past `replicas`. Retiring drains and stops each
    /// one, which takes a while, so the status says so first (the new
    /// replica count, the old instances still listed) and again after each:
    /// a Stop shows as stopping rather than as nothing until it is done.
    pub(super) fn scale_down(
        &mut self,
        def: &StackDef,
        insts: &[Inst],
        replicas: u32,
    ) -> Result<()> {
        let extra: Vec<&Inst> = insts
            .iter()
            .filter(|i| i.slot > replicas || i.slot == 0)
            .collect();
        if extra.is_empty() {
            return Ok(());
        }
        self.insts = insts.to_vec();
        self.state = "updating".into();
        self.message = Some(if replicas == 0 {
            "stopping".into()
        } else {
            format!("scaling down to {replicas}")
        });
        self.publish_status(def);
        for i in extra.iter().rev() {
            self.log(&format!("scaling down: removing {}", i.name));
            self.retire(&i.name)?;
            self.publish_status(def);
        }
        Ok(())
    }
}
