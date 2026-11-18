//! Project Genesis — Preemptive Kernel Task Scheduler.
//!
//! Provides round-robin scheduling for EL1 kernel threads with per-task
//! stacks, TrapFrame context switching, and NEON/FP state preservation.

use crate::arch::exceptions::TrapFrame;
use crate::mm::heap::SpinLock;

pub const MAX_TASKS: usize = 8;
pub const TASK_STACK_SIZE: usize = 16384; // 16 KiB = 4 pages

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TaskState {
    Unused,
    Ready,
    Running,
    Finished,
}

#[repr(C, align(16))]
#[derive(Clone, Copy, Debug)]
pub struct TaskFpContext {
    pub q: [u128; 32],
    pub fpsr: u64,
    pub fpcr: u64,
}

impl TaskFpContext {
    pub const fn empty() -> Self {
        Self {
            q: [0; 32],
            fpsr: 0,
            fpcr: 0,
        }
    }
}

pub struct Task {
    pub id: usize,
    pub state: TaskState,
    pub sp: u64,
    pub stack_top: u64,
    pub fp_ctx: TaskFpContext,
}

impl Task {
    pub const fn empty() -> Self {
        Self {
            id: 0,
            state: TaskState::Unused,
            sp: 0,
            stack_top: 0,
            fp_ctx: TaskFpContext::empty(),
        }
    }
}

pub struct Scheduler {
    pub tasks: [Task; MAX_TASKS],
    pub current: usize,
    pub active: bool,
    pub ticks: u64,
}

impl Scheduler {
    pub const fn new() -> Self {
        Self {
            tasks: [
                Task::empty(),
                Task::empty(),
                Task::empty(),
                Task::empty(),
                Task::empty(),
                Task::empty(),
                Task::empty(),
                Task::empty(),
            ],
            current: 0,
            active: false,
            ticks: 0,
        }
    }
}

pub static SCHEDULER: SpinLock<Scheduler> = SpinLock::new(Scheduler::new());

pub fn init_scheduler(main_stack_top: u64) {
    let mut sched = SCHEDULER.lock();
    sched.tasks[0].id = 0;
    sched.tasks[0].state = TaskState::Running;
    sched.tasks[0].stack_top = main_stack_top;
    sched.current = 0;
    sched.active = true;
    sched.ticks = 0;
}

pub fn spawn_task(entry: extern "C" fn(u64), arg: u64, stack_top: u64) -> Result<usize, ()> {
    let mut sched = SCHEDULER.lock();
    for idx in 1..MAX_TASKS {
        if sched.tasks[idx].state == TaskState::Unused || sched.tasks[idx].state == TaskState::Finished {
            let frame_ptr = (stack_top - core::mem::size_of::<TrapFrame>() as u64) as *mut TrapFrame;
            unsafe {
                (*frame_ptr).x = [0; 31];
                (*frame_ptr).x[0] = arg;
                (*frame_ptr).x[30] = task_exit_trampoline as *const () as u64;
                (*frame_ptr).spsr_el1 = 0x0000_0005; // EL1h with DAIF unmasked (all 0)
                (*frame_ptr).elr_el1 = entry as *const () as u64;
                (*frame_ptr).esr_el1 = 0;
                (*frame_ptr).far_el1 = 0;
                (*frame_ptr)._pad = 0;
            }

            sched.tasks[idx].id = idx;
            sched.tasks[idx].state = TaskState::Ready;
            sched.tasks[idx].sp = frame_ptr as u64;
            sched.tasks[idx].stack_top = stack_top;
            sched.tasks[idx].fp_ctx = TaskFpContext::empty();
            return Ok(idx);
        }
    }
    Err(())
}

#[inline]
pub unsafe fn save_fp_context(ctx: &mut TaskFpContext) {
    #[cfg(target_os = "uefi")]
    core::arch::asm!(
        "stp q0, q1, [{0}, #0]",
        "stp q2, q3, [{0}, #32]",
        "stp q4, q5, [{0}, #64]",
        "stp q6, q7, [{0}, #96]",
        "stp q8, q9, [{0}, #128]",
        "stp q10, q11, [{0}, #160]",
        "stp q12, q13, [{0}, #192]",
        "stp q14, q15, [{0}, #224]",
        "stp q16, q17, [{0}, #256]",
        "stp q18, q19, [{0}, #288]",
        "stp q20, q21, [{0}, #320]",
        "stp q22, q23, [{0}, #352]",
        "stp q24, q25, [{0}, #384]",
        "stp q26, q27, [{0}, #416]",
        "stp q28, q29, [{0}, #448]",
        "stp q30, q31, [{0}, #480]",
        "mrs {1}, fpsr",
        "mrs {2}, fpcr",
        in(reg) ctx.q.as_mut_ptr(),
        out(reg) ctx.fpsr,
        out(reg) ctx.fpcr,
        options(nostack)
    );
    #[cfg(not(target_os = "uefi"))]
    let _ = ctx;
}

#[inline]
pub unsafe fn restore_fp_context(ctx: &TaskFpContext) {
    #[cfg(target_os = "uefi")]
    core::arch::asm!(
        "ldp q0, q1, [{0}, #0]",
        "ldp q2, q3, [{0}, #32]",
        "ldp q4, q5, [{0}, #64]",
        "ldp q6, q7, [{0}, #96]",
        "ldp q8, q9, [{0}, #128]",
        "ldp q10, q11, [{0}, #160]",
        "ldp q12, q13, [{0}, #192]",
        "ldp q14, q15, [{0}, #224]",
        "ldp q16, q17, [{0}, #256]",
        "ldp q18, q19, [{0}, #288]",
        "ldp q20, q21, [{0}, #320]",
        "ldp q22, q23, [{0}, #352]",
        "ldp q24, q25, [{0}, #384]",
        "ldp q26, q27, [{0}, #416]",
        "ldp q28, q29, [{0}, #448]",
        "ldp q30, q31, [{0}, #480]",
        "msr fpsr, {1}",
        "msr fpcr, {2}",
        in(reg) ctx.q.as_ptr(),
        in(reg) ctx.fpsr,
        in(reg) ctx.fpcr,
        options(nostack)
    );
    #[cfg(not(target_os = "uefi"))]
    let _ = ctx;
}

pub fn scheduler_tick(current_frame: *mut TrapFrame) -> *mut TrapFrame {
    let mut sched = SCHEDULER.lock();
    if !sched.active {
        return current_frame;
    }
    sched.ticks += 1;

    let cur_idx = sched.current;
    sched.tasks[cur_idx].sp = current_frame as u64;

    #[cfg(target_os = "uefi")]
    unsafe {
        save_fp_context(&mut sched.tasks[cur_idx].fp_ctx);
    }

    if sched.tasks[cur_idx].state == TaskState::Running {
        sched.tasks[cur_idx].state = TaskState::Ready;
    }

    // Round-robin selection
    let mut next_idx = cur_idx;
    for i in 1..=MAX_TASKS {
        let cand = (cur_idx + i) % MAX_TASKS;
        if sched.tasks[cand].state == TaskState::Ready {
            next_idx = cand;
            break;
        }
    }

    sched.current = next_idx;
    sched.tasks[next_idx].state = TaskState::Running;

    #[cfg(target_os = "uefi")]
    unsafe {
        restore_fp_context(&sched.tasks[next_idx].fp_ctx);
    }

    sched.tasks[next_idx].sp as *mut TrapFrame
}

pub extern "C" fn task_exit_trampoline() -> ! {
    {
        let mut sched = SCHEDULER.lock();
        let cur = sched.current;
        sched.tasks[cur].state = TaskState::Finished;
    }
    loop {
        #[cfg(target_os = "uefi")]
        unsafe {
            core::arch::asm!("msr daifclr, #2", "wfi", options(nomem, nostack));
        }
        #[cfg(not(target_os = "uefi"))]
        core::hint::spin_loop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scheduler_initialization_and_task_spawning() {
        init_scheduler(0xFFFF_FFFF_8000_F000);
        let mut fake_stack = [0u8; 1024];
        let stack_top = (fake_stack.as_mut_ptr() as usize + fake_stack.len()) as u64;

        extern "C" fn test_fn(_arg: u64) {}

        let task_id = spawn_task(test_fn, 42, stack_top).expect("spawn task");
        assert_eq!(task_id, 1);

        let sched = SCHEDULER.lock();
        assert_eq!(sched.tasks[1].state, TaskState::Ready);
        let tf = unsafe { &*(sched.tasks[1].sp as *const TrapFrame) };
        assert_eq!(tf.x[0], 42);
        assert_eq!(tf.elr_el1, test_fn as *const () as u64);
        assert_eq!(tf.spsr_el1, 0x5);
    }
}
