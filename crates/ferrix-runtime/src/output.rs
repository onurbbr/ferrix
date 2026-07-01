//! Output helpers used by runtime-managed VM execution.

use std::{cell::RefCell, rc::Rc};

use ferrix_vm::{NullOutput, OutputWriter, VmError};

use crate::OutputMode;

/// Shared captured output buffer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CapturedOutput {
    buffer: Rc<RefCell<String>>,
}

impl CapturedOutput {
    /// Creates an empty captured output buffer.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the captured output.
    pub fn contents(&self) -> String {
        self.buffer.borrow().clone()
    }
}

impl OutputWriter for CapturedOutput {
    fn write_line(&mut self, line: &str) -> Result<(), VmError> {
        let mut buffer = self.buffer.borrow_mut();
        buffer.push_str(line);
        buffer.push('\n');
        Ok(())
    }
}

/// Installs the selected output writer into a VM and returns a capture handle.
pub(crate) fn install_output(vm: &mut ferrix_vm::Vm, mode: OutputMode) -> Option<CapturedOutput> {
    match mode {
        OutputMode::Capture => {
            let capture = CapturedOutput::new();
            vm.set_output_writer(capture.clone());
            Some(capture)
        }
        OutputMode::Null => {
            vm.set_output_writer(NullOutput);
            None
        }
    }
}
