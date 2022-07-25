//! Process management syscalls
use crate::mm::{translated_refmut, translated_ref, translated_str, VirtAddr, MapPermission};

use crate::config::{PAGE_SIZE, BIG_STRIDE};

use crate::task::{
    add_task, current_task, current_user_token, exit_current_and_run_next, suspend_current_and_run_next, TaskStatus,
    get_cur_task_info, get_tcb_ref_mut
};
use crate::fs::{open_file, OpenFlags};
use crate::timer::get_time_us;
use alloc::sync::Arc;
use alloc::vec::Vec;
use crate::config::MAX_SYSCALL_NUM;
use alloc::string::String;

#[repr(C)]
#[derive(Debug)]
pub struct TimeVal {
    pub sec: usize,
    pub usec: usize,
}

#[derive(Clone, Copy)]
pub struct TaskInfo {
    pub status: TaskStatus,
    pub syscall_times: [u32; MAX_SYSCALL_NUM],
    pub time: usize,
}

pub fn sys_exit(exit_code: i32) -> ! {
    debug!("[kernel] Application exited with code {}", exit_code);
    exit_current_and_run_next(exit_code);
    panic!("Unreachable in sys_exit!");
}

/// current task gives up resources for other tasks
pub fn sys_yield() -> isize {
    suspend_current_and_run_next();
    0
}

pub fn sys_getpid() -> isize {
    current_task().unwrap().pid.0 as isize
}

/// Syscall Fork which returns 0 for child process and child_pid for parent process
pub fn sys_fork() -> isize {
    let current_task = current_task().unwrap();
    let new_task = current_task.fork();
    let new_pid = new_task.pid.0;
    // modify trap context of new_task, because it returns immediately after switching
    let trap_cx = new_task.inner_exclusive_access().get_trap_cx();
    // we do not have to move to next instruction since we have done it before
    // for child process, fork returns 0
    trap_cx.x[10] = 0;
    // add new task to scheduler
    add_task(new_task);
    new_pid as isize
}

/// Syscall Exec which accepts the elf path
pub fn sys_exec(path: *const u8) -> isize {
    let token = current_user_token();
    let path = translated_str(token, path);
    if let Some(app_inode) = open_file(path.as_str(), OpenFlags::RDONLY) {
        let all_data = app_inode.read_all();
        let task = current_task().unwrap();
        task.exec(all_data.as_slice());
        0
    } else {
        -1
    }
}


/// If there is not a child process whose pid is same as given, return -1.
/// Else if there is a child process but it is still running, return -2.
pub fn sys_waitpid(pid: isize, exit_code_ptr: *mut i32) -> isize {
    let task = current_task().unwrap();
    // find a child process

    // ---- access current TCB exclusively
    let mut inner = task.inner_exclusive_access();
    if !inner
        .children
        .iter()
        .any(|p| pid == -1 || pid as usize == p.getpid())
    {
        return -1;
        // ---- release current PCB
    }
    let pair = inner.children.iter().enumerate().find(|(_, p)| {
        // ++++ temporarily access child PCB lock exclusively
        p.inner_exclusive_access().is_zombie() && (pid == -1 || pid as usize == p.getpid())
        // ++++ release child PCB
    });
    if let Some((idx, _)) = pair {
        let child = inner.children.remove(idx);
        // confirm that child will be deallocated after removing from children list
        assert_eq!(Arc::strong_count(&child), 1);
        let found_pid = child.getpid();
        // ++++ temporarily access child TCB exclusively
        let exit_code = child.inner_exclusive_access().exit_code;
        // ++++ release child PCB
        *translated_refmut(inner.memory_set.token(), exit_code_ptr) = exit_code;
        found_pid as isize
    } else {
        -2
    }
    // ---- release current PCB lock automatically
}

// YOUR JOB: 引入虚地址后重写 sys_get_time
pub fn sys_get_time(_ts: *mut TimeVal, _tz: usize) -> isize {
    let us = get_time_us();
    let t = translated_refmut(current_user_token(), _ts);

    t.sec = us / 1000000;
    t.usec = us % 1000000;
    
    0
}

// YOUR JOB: 引入虚地址后重写 sys_task_info
pub fn sys_task_info(ti: *mut TaskInfo) -> isize {
    let (status, stats) = get_cur_task_info();
    let t = translated_refmut(current_user_token(), ti);

    // println!("addr:={}", t as *const TaskInfo as  usize);
    // println!("size:={}", core::mem::size_of::<TaskInfo>());

    *t = TaskInfo{
        status,
        syscall_times: stats.syscall_times.clone(),
        time: (get_time_us() - stats.first_run_time) / 1000,
    };
    0
}

// YOUR JOB: 实现sys_set_priority，为任务添加优先级
pub fn sys_set_priority(prio: isize) -> isize {
    // info!("set pri ------------");
    if prio < 2 {
        return -1;
    }
    let task = current_task().unwrap();
    let mut t = BIG_STRIDE / (prio as u64);
    
    if t == 0 {
        t = 1;
    }
    task.inner_exclusive_access().stride = t;

    // println!("pid={} set pri = {}, stride= {}", task.pid.0, prio, t);

    prio
}

// YOUR JOB: 扩展内核以实现 sys_mmap 和 sys_munmap
pub fn sys_mmap(_start: usize, _len: usize, _port: usize) -> isize {
    if _start & (PAGE_SIZE-1) != 0 {
        // 没有按照页对齐
        return -1;
    }

    if _port & (!0x07) != 0 || (_port & 0x07) == 0 {
        return -1;
    } 

    let start_va: VirtAddr = VirtAddr::from(_start).floor().into();
    let end_va: VirtAddr = VirtAddr::from(_start + _len).ceil().into();


    let mut permission = MapPermission::empty();
    permission.set(MapPermission::U, true);

    if _port & 0x01 != 0 {
        permission.set(MapPermission::R, true);
    }

    if _port & 0x02 != 0 {
        permission.set(MapPermission::W, true);
    }

    if _port & 0x04 != 0 {
        permission.set(MapPermission::X, true);
    }


    if !get_tcb_ref_mut(|tcb| {
        tcb.memory_set.mmap(start_va.into(), end_va.into(), permission)
    })  {
        return -1;
    }
    
    0
}

pub fn sys_munmap(_start: usize, _len: usize) -> isize {
    if !get_tcb_ref_mut(|tcb| {
        tcb.memory_set.munmap(_start.into(), _len.into())
    })  {
        return -1;
    }
    0
}

//
// YOUR JOB: 实现 sys_spawn 系统调用
// ALERT: 注意在实现 SPAWN 时不需要复制父进程地址空间，SPAWN != FORK + EXEC 
pub fn sys_spawn(path: *const u8) -> isize {

    let token = current_user_token();
    let path = translated_str(token, path);


    if let Some(app_inode) = open_file(path.as_str(), OpenFlags::RDONLY) {
        let all_data = app_inode.read_all();
        let task = current_task().unwrap();
        let new_task = task.spawn(&all_data);
        let new_pid = new_task.pid.0;
        let trap_cx = new_task.inner_exclusive_access().get_trap_cx();
        trap_cx.x[10] = 0;
        add_task(new_task);
        // info!("spawn {}, pid={}", path, new_pid);
        new_pid as isize
    } else {
        -1
    }
}
