//! Reader-pool concurrency tests.

use std::sync::Arc;

use rusqlite::Connection;

use clarion_storage::{ReaderPool, pragma, schema};

fn prepared_db(dir: &tempfile::TempDir) -> std::path::PathBuf {
    let path = dir.path().join("clarion.db");
    let mut conn = Connection::open(&path).expect("open");
    pragma::apply_write_pragmas(&conn).expect("write pragmas");
    schema::apply_migrations(&mut conn).expect("migrate");
    path
}

#[tokio::test]
async fn two_readers_run_concurrently() {
    let dir = tempfile::tempdir().unwrap();
    let path = prepared_db(&dir);
    let pool = Arc::new(ReaderPool::open(&path, 2).expect("pool"));

    let p1 = pool.clone();
    let p2 = pool.clone();
    let (a, b) = tokio::join!(
        p1.with_reader(|conn| {
            let n: i64 = conn.query_row("SELECT 1", [], |row| row.get(0))?;
            Ok(n)
        }),
        p2.with_reader(|conn| {
            let n: i64 = conn.query_row("SELECT 2", [], |row| row.get(0))?;
            Ok(n)
        })
    );
    assert_eq!(a.unwrap(), 1);
    assert_eq!(b.unwrap(), 2);
}

#[tokio::test]
async fn reader_sees_committed_data() {
    let dir = tempfile::tempdir().unwrap();
    let path = prepared_db(&dir);

    // Pre-seed a runs row via a one-shot blocking connection.
    {
        let conn = Connection::open(&path).unwrap();
        pragma::apply_write_pragmas(&conn).unwrap();
        conn.execute(
            "INSERT INTO runs (id, started_at, completed_at, config, stats, status) \
             VALUES (?1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), NULL, '{}', '{}', 'running')",
            rusqlite::params!["run-1"],
        )
        .unwrap();
    }

    let pool = ReaderPool::open(&path, 2).expect("pool");
    let status: String = pool
        .with_reader(|conn| {
            let status: String =
                conn.query_row("SELECT status FROM runs WHERE id = 'run-1'", [], |row| {
                    row.get(0)
                })?;
            Ok(status)
        })
        .await
        .unwrap();
    assert_eq!(status, "running");
}

/// Gate for `pool_queues_when_exhausted_and_proceeds_after_release`.
///
/// The first reader flips `acquired` once it holds the connection, then waits
/// on `cv_released` for the main task's release signal. Lets the test
/// synchronise precisely, without wall-clock guesses about "when has the
/// first reader probably acquired by now."
#[derive(Default)]
struct ReaderPoolGate {
    acquired: std::sync::Mutex<bool>,
    released: std::sync::Mutex<bool>,
    cv_acquired: std::sync::Condvar,
    cv_released: std::sync::Condvar,
}

#[tokio::test]
async fn pool_queues_when_exhausted_and_proceeds_after_release() {
    use tokio::time::{Duration, timeout};

    let dir = tempfile::tempdir().unwrap();
    let path = prepared_db(&dir);
    // max_size = 1 makes the exhaustion scenario trivial to construct.
    let pool = Arc::new(ReaderPool::open(&path, 1).expect("pool"));

    let gate = Arc::new(ReaderPoolGate::default());

    let pool_for_hold = pool.clone();
    let gate_for_hold = gate.clone();
    let held = tokio::spawn(async move {
        pool_for_hold
            .with_reader(move |conn| {
                // Prove the connection was actually acquired.
                let _: i64 = conn.query_row("SELECT 1", [], |row| row.get(0))?;
                // Signal the main task that we hold the connection, then park
                // (sync-wait) until it signals release. `.await` inside
                // interact() is not permitted — this is a blocking thread.
                {
                    let mut a = gate_for_hold.acquired.lock().unwrap();
                    *a = true;
                    gate_for_hold.cv_acquired.notify_one();
                }
                let mut r = gate_for_hold.released.lock().unwrap();
                while !*r {
                    r = gate_for_hold.cv_released.wait(r).unwrap();
                }
                Ok::<_, clarion_storage::StorageError>(())
            })
            .await
    });

    // Wait deterministically for the first reader to acquire the connection.
    // (A short timeout, but the gate signalling is precise — no wall-clock
    // guess about scheduling.)
    let gate_wait = gate.clone();
    tokio::task::spawn_blocking(move || {
        let mut a = gate_wait.acquired.lock().unwrap();
        while !*a {
            let (guard, _) = gate_wait
                .cv_acquired
                .wait_timeout(a, Duration::from_secs(2))
                .unwrap();
            a = guard;
            if *a {
                break;
            }
        }
        assert!(*a, "first reader failed to acquire within 2s");
    })
    .await
    .unwrap();

    // Second reader: should block on the exhausted pool. Spawn it and wait
    // until deadpool reports it as a queued waiter — then assert it is not
    // finished. Polling waiting_count avoids the 100 ms wall-clock guess
    // that bug clarion-b5b1029f5a flagged as flaky under CI load.
    let pool_for_wait = pool.clone();
    let second_handle = tokio::spawn(async move {
        pool_for_wait
            .with_reader(|conn| {
                let n: i64 = conn.query_row("SELECT 2", [], |row| row.get(0))?;
                Ok(n)
            })
            .await
    });
    timeout(Duration::from_secs(2), async {
        loop {
            if pool.waiting_count() >= 1 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("second reader should have reached the pool wait-list within 2s");
    assert!(
        !second_handle.is_finished(),
        "second reader must be parked while first holds the only connection"
    );

    // Release the first reader.
    {
        let mut r = gate.released.lock().unwrap();
        *r = true;
        gate.cv_released.notify_one();
    }

    // Both readers must complete promptly.
    let second = timeout(Duration::from_secs(2), second_handle)
        .await
        .expect("second reader should eventually complete within 2s")
        .expect("second reader join")
        .expect("second reader's query should succeed");
    assert_eq!(second, 2);
    held.await.unwrap().unwrap();
}

#[tokio::test]
async fn reader_error_propagates_and_connection_returns_to_pool() {
    let dir = tempfile::tempdir().unwrap();
    let path = prepared_db(&dir);
    let pool = ReaderPool::open(&path, 2).expect("pool");

    // First call returns an error from the closure.
    let err_result: Result<i64, _> = pool
        .with_reader(|conn| {
            let _: i64 = conn.query_row("SELECT 1", [], |row| row.get(0))?;
            // Deliberate invalid SQL to force an error in the closure.
            conn.query_row("SELECT * FROM non_existent_table", [], |row| row.get(0))
                .map_err(clarion_storage::StorageError::from)
        })
        .await;

    assert!(err_result.is_err(), "expected closure error to propagate");
    assert!(matches!(
        err_result.unwrap_err(),
        clarion_storage::StorageError::Sqlite(_)
    ));

    // Second call on the same pool must succeed — proves the connection
    // from the first call was returned to the pool cleanly.
    let ok: i64 = pool
        .with_reader(|conn| {
            let n: i64 = conn.query_row("SELECT 42", [], |row| row.get(0))?;
            Ok(n)
        })
        .await
        .expect("subsequent reader after an error should succeed");
    assert_eq!(ok, 42);
}

#[tokio::test]
async fn reader_panic_is_caught_as_pool_interact_and_pool_remains_usable() {
    let dir = tempfile::tempdir().unwrap();
    let path = prepared_db(&dir);
    let pool = ReaderPool::open(&path, 2).expect("pool");

    // Closure that panics inside the interact() block.
    let panic_result: Result<i64, _> = pool
        .with_reader(|_conn| {
            panic!("deliberate test panic inside reader closure");
        })
        .await;

    assert!(panic_result.is_err(), "expected panic to surface as error");
    assert!(matches!(
        panic_result.unwrap_err(),
        clarion_storage::StorageError::PoolInteract(_)
    ));

    // Pool remains usable — deadpool recycles the poisoned connection or
    // discards it and creates a fresh one. Subsequent calls must succeed.
    let ok: i64 = pool
        .with_reader(|conn| {
            let n: i64 = conn.query_row("SELECT 99", [], |row| row.get(0))?;
            Ok(n)
        })
        .await
        .expect("subsequent reader after a panic should succeed");
    assert_eq!(ok, 99);
}
