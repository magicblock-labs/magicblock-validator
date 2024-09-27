use rayon::{ThreadPool, ThreadPoolBuilder};
pub mod rayon_prelude {
    pub use rayon::prelude::*;
}

#[allow(dead_code)] // used in tests
pub fn iteration_count() -> u32 {
    std::env::var("TEST_ITERATION_COUNT")
        .unwrap_or_else(|_| "1".to_string())
        .parse()
        .unwrap()
}

#[allow(dead_code)] // used in tests
/// Resolves concurrency and returns an initialized threadpool if
/// concurrency > 1
pub fn iteration_thread_pool() -> (Option<ThreadPool>, u32) {
    let concurrency = iteration_concurrency();
    if concurrency == 1 {
        (None, concurrency)
    } else {
        (
            Some(
                ThreadPoolBuilder::new()
                    .num_threads(concurrency as usize)
                    .build()
                    .unwrap(),
            ),
            concurrency,
        )
    }
}

fn iteration_concurrency() -> u32 {
    std::env::var("TEST_ITERATION_CONCURRENCY")
        .unwrap_or_else(|_| "1".to_string())
        .parse()
        .unwrap()
}

#[macro_export]
macro_rules! function_name {
    () => {{
        fn f() {}
        fn type_name_of<T>(_: T) -> &'static str {
            std::any::type_name::<T>()
        }
        let name = type_name_of(f);
        &name[..name.len() - 3]
    }};
}

#[macro_export]
macro_rules! run_test {
    ($test_body:block) => {
        let total_completed: ::std::sync::atomic::AtomicUsize =
            ::std::sync::atomic::AtomicUsize::new(0);

        init_logger!();

        let test_name = $crate::function_name!();
        let test = || $test_body;

        let iterations = $crate::iteration_count();
        let (thread_pool, concurrency) = $crate::iteration_thread_pool();

        info!(
            "==== {}: (ITER: {}, CONCURRENCY: {}) ====",
            test_name, iterations, concurrency
        );

        macro_rules! do_run {
            ($i:ident) => {
                info!("Start {}[{}]", test_name, $i);
                test();
                info!(
                    "Completed {}[{}] - completed {}/{}",
                    test_name,
                    format!("{:04}", $i),
                    format!(
                        "{:04}",
                        total_completed.fetch_add(
                            1,
                            ::std::sync::atomic::Ordering::Relaxed
                        ) + 1
                    ),
                    format!("{:04}", iterations)
                );
            };
        };

        if let Some(thread_pool) = thread_pool {
            use $crate::rayon_prelude::*;
            thread_pool.install(|| {
                (1..=iterations).into_par_iter().for_each(|i| {
                    do_run!(i);
                });
            });
        } else {
            for i in 1..=iterations {
                do_run!(i);
            }
        }
    };
}
