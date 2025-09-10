use crate::tasks::{ArgsTask, BufferTask};

pub trait Visitor {
    fn visit_args_task(&mut self, task: &ArgsTask);
    fn visit_buffer_task(&mut self, task: &BufferTask);

    fn visit_args_task_mut(&mut self, task: &mut ArgsTask);
    fn visit_buffer_task_mut(&mut self, task: &mut BufferTask);
}
