use crate::Result;
use crate::state::{ApplyState, StateFile};

pub(crate) trait StateWriter {
    fn write_state(&mut self, state: &mut ApplyState) -> Result<()>;
    fn remove_state(&mut self) -> Result<()>;
}

pub(crate) struct LockedStateWriter {
    state_file: Option<StateFile>,
}

pub(crate) struct NoopStateWriter;

impl LockedStateWriter {
    pub(crate) fn new(state_file: StateFile) -> Self {
        Self {
            state_file: Some(state_file),
        }
    }
}

impl StateWriter for LockedStateWriter {
    fn write_state(&mut self, state: &mut ApplyState) -> Result<()> {
        self.state_file
            .as_mut()
            .expect("locked state writer has state file")
            .write_state(state)
    }

    fn remove_state(&mut self) -> Result<()> {
        self.state_file
            .take()
            .expect("locked state writer has state file")
            .remove_if_exists()
    }
}

impl StateWriter for NoopStateWriter {
    fn write_state(&mut self, _state: &mut ApplyState) -> Result<()> {
        Ok(())
    }

    fn remove_state(&mut self) -> Result<()> {
        Ok(())
    }
}
