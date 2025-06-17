//! Cron-like scheduling for Solana validator maintenance tasks
//! 
//! This example demonstrates how to implement cron-style scheduling for:
//! - Database maintenance and compaction
//! - Ledger cleanup and archival
//! - Metrics reporting and alerting
//! - Account data synchronization

use magicblock_time_delta::TimeDelta;
use std::collections::HashMap;

/// A scheduled task that can be executed periodically
pub trait ScheduledTask {
    fn execute(&mut self) -> TaskResult;
    fn name(&self) -> &str;
}

#[derive(Debug)]
pub enum TaskResult {
    Success,
    Warning(String),
    Error(String),
}

/// Cron-style scheduler for validator maintenance tasks
pub struct ValidatorScheduler {
    tasks: HashMap<String, Box<dyn ScheduledTask>>,
    timers: HashMap<String, TimeDelta>,
}

impl ValidatorScheduler {
    pub fn new() -> Self {
        Self {
            tasks: HashMap::new(),
            timers: HashMap::new(),
        }
    }

    /// Add a task to be executed every `interval_ms` milliseconds
    pub fn schedule_task(&mut self, task: Box<dyn ScheduledTask>, interval_ms: u64) {
        let name = task.name().to_string();
        self.timers.insert(name.clone(), TimeDelta::new(interval_ms));
        self.tasks.insert(name, task);
    }

    /// Process all scheduled tasks, executing those that are due
    pub fn tick(&mut self) -> Vec<TaskExecutionReport> {
        let mut reports = Vec::new();

        for (name, timer) in self.timers.iter_mut() {
            if timer.should_tick() {
                let tick_count = timer.tick_count();
                
                if let Some(task) = self.tasks.get_mut(name) {
                    let result = task.execute();
                    reports.push(TaskExecutionReport {
                        task_name: name.clone(),
                        result,
                        ticks_elapsed: tick_count,
                    });
                }
                
                timer.update();
            }
        }

        reports
    }

    /// Get status of all scheduled tasks
    pub fn get_status(&self) -> Vec<TaskStatus> {
        self.timers.iter().map(|(name, timer)| {
            TaskStatus {
                name: name.clone(),
                time_to_next_execution: timer.time_to_next_tick(),
                interval: timer.tick_interval(),
            }
        }).collect()
    }
}

#[derive(Debug)]
pub struct TaskExecutionReport {
    pub task_name: String,
    pub result: TaskResult,
    pub ticks_elapsed: u64,
}

#[derive(Debug)]
pub struct TaskStatus {
    pub name: String,
    pub time_to_next_execution: u64,
    pub interval: u64,
}

// Example validator maintenance tasks

/// Compacts the accounts database to reclaim space
struct AccountsDbCompaction {
    last_size_mb: u64,
}

impl AccountsDbCompaction {
    fn new() -> Self {
        Self { last_size_mb: 1000 }
    }
}

impl ScheduledTask for AccountsDbCompaction {
    fn execute(&mut self) -> TaskResult {
        // Simulate database compaction
        let size_before = self.last_size_mb;
        self.last_size_mb = (size_before as f64 * 0.85) as u64; // 15% reduction
        let saved_mb = size_before - self.last_size_mb;
        
        println!("🗜️  Accounts DB Compaction: {}MB -> {}MB (saved {}MB)", 
                size_before, self.last_size_mb, saved_mb);
        
        if saved_mb > 50 {
            TaskResult::Success
        } else {
            TaskResult::Warning("Low space savings from compaction".to_string())
        }
    }

    fn name(&self) -> &str {
        "accounts_db_compaction"
    }
}

/// Cleans up old ledger data beyond retention period
struct LedgerCleanup {
    slots_cleaned: u64,
}

impl LedgerCleanup {
    fn new() -> Self {
        Self { slots_cleaned: 0 }
    }
}

impl ScheduledTask for LedgerCleanup {
    fn execute(&mut self) -> TaskResult {
        // Simulate ledger cleanup
        let slots_to_clean = 100 + (self.slots_cleaned % 50);
        self.slots_cleaned += slots_to_clean;
        
        println!("🧹 Ledger Cleanup: Removed {} old slots (total: {} slots cleaned)", 
                slots_to_clean, self.slots_cleaned);
        
        TaskResult::Success
    }

    fn name(&self) -> &str {
        "ledger_cleanup"
    }
}

/// Reports validator metrics to monitoring systems
struct MetricsReporter {
    reports_sent: u64,
}

impl MetricsReporter {
    fn new() -> Self {
        Self { reports_sent: 0 }
    }
}

impl ScheduledTask for MetricsReporter {
    fn execute(&mut self) -> TaskResult {
        self.reports_sent += 1;
        
        // Simulate metrics collection and reporting
        let tps = 2000 + (self.reports_sent * 100) % 500; // Varying TPS
        let slot_height = 50000 + self.reports_sent * 10;
        
        println!("📊 Metrics Report #{}: TPS={}, Slot={}, Memory={}MB", 
                self.reports_sent, tps, slot_height, 1500 + (self.reports_sent % 200));
        
        // Simulate occasional network issues
        if self.reports_sent % 7 == 0 {
            TaskResult::Warning("Metrics upload took longer than expected".to_string())
        } else {
            TaskResult::Success
        }
    }

    fn name(&self) -> &str {
        "metrics_reporter"
    }
}

/// Synchronizes account data with other validators
struct AccountSync {
    sync_count: u64,
}

impl AccountSync {
    fn new() -> Self {
        Self { sync_count: 0 }
    }
}

impl ScheduledTask for AccountSync {
    fn execute(&mut self) -> TaskResult {
        self.sync_count += 1;
        
        let accounts_synced = 50 + (self.sync_count % 30);
        println!("🔄 Account Sync #{}: Synchronized {} accounts with cluster", 
                self.sync_count, accounts_synced);
        
        // Simulate occasional sync conflicts
        if self.sync_count % 5 == 0 {
            TaskResult::Warning("Some accounts had sync conflicts (resolved)".to_string())
        } else {
            TaskResult::Success
        }
    }

    fn name(&self) -> &str {
        "account_sync"
    }
}

fn main() {
    println!("=== Magicblock Validator Cron Scheduler ===\n");

    let mut scheduler = ValidatorScheduler::new();

    // Schedule maintenance tasks at different intervals
    scheduler.schedule_task(Box::new(AccountsDbCompaction::new()), 15000);  // Every 15 seconds
    scheduler.schedule_task(Box::new(LedgerCleanup::new()), 30000);         // Every 30 seconds  
    scheduler.schedule_task(Box::new(MetricsReporter::new()), 5000);        // Every 5 seconds
    scheduler.schedule_task(Box::new(AccountSync::new()), 10000);           // Every 10 seconds

    println!("Scheduled tasks:");
    for status in scheduler.get_status() {
        println!("  - {} (every {}ms, next in {}ms)", 
                status.name, status.interval, status.time_to_next_execution);
    }
    println!();

    // Simulate validator running for 60 seconds
    let start_time = std::time::Instant::now();
    let mut tick_count = 0;

    while start_time.elapsed().as_secs() < 60 {
        let reports = scheduler.tick();
        
        for report in reports {
            tick_count += 1;
            print!("[{:03}] ", tick_count);
            
            match report.result {
                TaskResult::Success => print!("✅ "),
                TaskResult::Warning(_) => print!("⚠️  "),
                TaskResult::Error(_) => print!("❌ "),
            }
            
            match report.result {
                TaskResult::Success => {},
                TaskResult::Warning(msg) => println!("    Warning: {}", msg),
                TaskResult::Error(msg) => println!("    Error: {}", msg),
            }
            
            if report.ticks_elapsed > 1 {
                println!("    (Caught up {} missed executions)", report.ticks_elapsed);
            }
        }

        // Simulate validator work
        std::thread::sleep(std::time::Duration::from_millis(500));
    }

    println!("\n=== Scheduler completed 60-second run ===");
    
    // Final status report
    println!("\nFinal task status:");
    for status in scheduler.get_status() {
        println!("  - {}: next execution in {}ms", status.name, status.time_to_next_execution);
    }
}
