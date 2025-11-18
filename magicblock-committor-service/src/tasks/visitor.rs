use crate::tasks::Task;

pub trait Visitor {
    fn visit_task(&mut self, task: &Task);
}
