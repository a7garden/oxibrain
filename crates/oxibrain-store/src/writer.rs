//! One writer actor per store. All writes serialize through an owned thread
//! holding the write connection (DESIGN.md §13.1, P8).

use crate::Store;
use crate::sql_err;
use oxibrain_ports::BrainError;
use std::sync::mpsc;
use std::thread;

/// A write operation: a closure run inside the writer thread, on the write connection, in a tx.
pub type WriteOp = Box<dyn FnOnce(&rusqlite::Connection) -> Result<(), BrainError> + Send>;

/// Commands to the writer thread.
enum Cmd {
    Write(WriteOp),
    Flush(mpsc::Sender<Result<(), BrainError>>),
    Stop,
}

pub struct WriterActor {
    tx: mpsc::Sender<Cmd>,
    handle: Option<thread::JoinHandle<()>>,
}

impl WriterActor {
    /// Spawn the writer thread. Takes ownership of the store's write connection and its
    /// advisory lock; the lock is held for the actor's lifetime (P8: one writer per store).
    pub fn spawn(store: Store) -> Self {
        let (tx, rx) = mpsc::channel::<Cmd>();
        let handle = thread::Builder::new()
            .name("oxibrain-writer".into())
            .spawn(move || {
                let (mut conn, _lock) = store.into_parts();
                loop {
                    match rx.recv() {
                        Ok(Cmd::Write(op)) => {
                            if let Err(e) = run_in_tx(&mut conn, op) {
                                tracing::warn!(error = %e, "write op failed");
                            }
                        }
                        Ok(Cmd::Flush(reply)) => {
                            let _ = reply.send(Ok(()));
                        }
                        Ok(Cmd::Stop) | Err(_) => break,
                    }
                }
            })
            .expect("spawn writer");
        Self {
            tx,
            handle: Some(handle),
        }
    }

    pub fn submit(&self, op: WriteOp) -> Result<(), BrainError> {
        self.tx
            .send(Cmd::Write(op))
            .map_err(|_| BrainError::Storage("writer thread gone".into()))
    }

    /// Block until the writer has processed everything submitted so far.
    pub fn flush(&self) -> Result<(), BrainError> {
        let (tx, rx) = mpsc::channel();
        self.tx
            .send(Cmd::Flush(tx))
            .map_err(|_| BrainError::Storage("writer thread gone".into()))?;
        rx.recv()
            .map_err(|_| BrainError::Storage("writer thread gone".into()))?
    }

    pub fn stop(&mut self) {
        let _ = self.tx.send(Cmd::Stop);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for WriterActor {
    fn drop(&mut self) {
        self.stop();
    }
}

fn run_in_tx(conn: &mut rusqlite::Connection, op: WriteOp) -> Result<(), BrainError> {
    let tx = conn.transaction().map_err(sql_err)?;
    op(&tx)?;
    tx.commit().map_err(sql_err)?;
    Ok(())
}
