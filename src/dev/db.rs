//! Facilities for managing a local database for development

use crate::dev::poll;
use anyhow::Context;
use core::ops::Deref;
use std::ffi::OsStr;
use std::fmt;
use std::path::Path;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tempfile::tempdir;
use tempfile::TempDir;
use thiserror::Error;

/// Default for how long to wait for CockroachDB to report its listening URL
const COCKROACHDB_START_TIMEOUT_DEFAULT: Duration = Duration::from_secs(30);

/**
 * Builder for [`CockroachStarter`] that supports setting some command-line
 * arguments for the `cockroach start-single-node` command
 *
 * Without customizations, this will run `cockroach start-single-node --insecure
 * --listen-addr=127.0.0.1:0 --http-addr=:0`.
 *
 * It's useful to support running this concurrently (as in the test suite).  To
 * support this, we allow CockroachDB to choose its listening ports.  To figure
 * out which ports it chose, we also use the --listening-url-file option to have
 * it write the URL to a file in a temporary directory.  The Drop
 * implementations for `CockroachStarter` and `CockroachInstance` will ensure
 * that this directory gets cleaned up as long as this program exits normally.
 */
#[derive(Debug)]
pub struct CockroachStarterBuilder {
    /// optional value for the --store-dir option
    store_dir: Option<PathBuf>,
    /// command-line arguments, mirrored here for reporting
    args: Vec<String>,
    /// describes the command line that we're going to execute
    cmd_builder: tokio::process::Command,
    /// how long to wait for CockroachDB to report itself listening
    start_timeout: Duration,
    /// redirect stdout and stderr to files
    redirect_stdio: bool,
}

impl CockroachStarterBuilder {
    pub fn new() -> CockroachStarterBuilder {
        CockroachStarterBuilder::new_with_cmd("cockroach")
    }

    fn new_with_cmd(cmd: &str) -> CockroachStarterBuilder {
        let mut builder = CockroachStarterBuilder {
            store_dir: None,
            args: vec![String::from(cmd)],
            cmd_builder: tokio::process::Command::new(cmd),
            start_timeout: COCKROACHDB_START_TIMEOUT_DEFAULT,
            redirect_stdio: false,
        };

        /*
         * We use single-node insecure mode listening only on localhost.  We
         * consider this secure enough for development (including the test
         * suite), though it does allow anybody on the system to do anything
         * with this database (including fill up all disk space).  (It wouldn't
         * be unreasonable to secure this with certificates even though we're
         * on localhost.
         *
         * If we decide to let callers customize various listening addresses, we
         * should be careful about making it too easy to generate a more
         * insecure configuration.
         */
        builder
            .arg("start-single-node")
            .arg("--insecure")
            .arg("--listen-addr=127.0.0.1:0")
            .arg("--http-addr=:0");
        builder
    }

    pub fn redirect_stdio_to_files(&mut self) -> &mut Self {
        self.redirect_stdio = true;
        self
    }

    pub fn start_timeout(&mut self, duration: &Duration) -> &mut Self {
        self.start_timeout = *duration;
        self
    }

    /**
     * Sets the `--store-dir` command-line argument to `store_dir`
     *
     * This is where the database will store all of its on-disk data.  If this
     * isn't specified, CockroachDB will be configured to store data into a
     * temporary directory that will be cleaned up on Drop of
     * [`CockroachStarter`] or [`CockroachInstance`].
     */
    pub fn store_dir<P: AsRef<Path>>(mut self, store_dir: P) -> Self {
        self.store_dir.replace(store_dir.as_ref().to_owned());
        self
    }

    fn redirect_file(
        &self,
        temp_dir_path: &Path,
        label: &str,
    ) -> Result<std::fs::File, anyhow::Error> {
        let out_path = temp_dir_path.join(label);
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&out_path)
            .with_context(|| format!("open \"{}\"", out_path.display()))
    }

    /**
     * Starts CockroachDB using the configured command-line arguments
     *
     * This will create a temporary directory for the listening URL file (see
     * above) and potentially the database store directory (if `store_dir()`
     * was never called).
     */
    pub fn build(mut self) -> Result<CockroachStarter, anyhow::Error> {
        /*
         * We always need a temporary directory, if for no other reason than to
         * put the listen-url file.  (It would be nice if the subprocess crate
         * allowed us to open a pipe stream to the child other than stdout or
         * stderr, although there may not be a portable means to identify it to
         * CockroachDB on the command line.)
         *
         * TODO Maybe it would be more ergonomic to use a well-known temporary
         * directory rather than a random one.  That way, we can warn the user
         * if they start up two of them, and we can also clean up after unclean
         * shutdowns.
         */
        let temp_dir =
            tempdir().with_context(|| "creating temporary directory")?;
        let store_dir = self
            .store_dir
            .as_ref()
            .map(|s| s.as_os_str().to_owned())
            .unwrap_or_else(|| {
                CockroachStarterBuilder::temp_path(&temp_dir, "data")
                    .into_os_string()
            });
        let listen_url_file =
            CockroachStarterBuilder::temp_path(&temp_dir, "listen-url");
        self.arg("--store")
            .arg(store_dir)
            .arg("--listening-url-file")
            .arg(listen_url_file.as_os_str().to_owned());

        if self.redirect_stdio {
            let temp_dir_path = temp_dir.path();
            self.cmd_builder.stdout(Stdio::from(
                self.redirect_file(temp_dir_path, "cockroachdb_stdout")?,
            ));
            self.cmd_builder.stderr(Stdio::from(
                self.redirect_file(temp_dir_path, "cockroachdb_stderr")?,
            ));
        }

        Ok(CockroachStarter {
            temp_dir,
            listen_url_file,
            args: self.args,
            cmd_builder: self.cmd_builder,
            start_timeout: self.start_timeout,
        })
    }

    /**
     * Convenience wrapper for self.cmd_builder.arg() that records the arguments
     * so that we can print out the command line before we run it
     */
    fn arg<S: AsRef<OsStr>>(&mut self, arg: S) -> &mut Self {
        let arg = arg.as_ref();
        self.args.push(arg.to_string_lossy().to_string());
        self.cmd_builder.arg(arg);
        self
    }

    /**
     * Convenience for constructing a path name in a given temporary directory
     */
    fn temp_path<S: AsRef<str>>(tempdir: &TempDir, file: S) -> PathBuf {
        let mut pathbuf = tempdir.path().to_owned();
        pathbuf.push(file.as_ref());
        pathbuf
    }
}

/**
 * Manages execution of the `cockroach` command in order to start a CockroachDB
 * instance
 *
 * To use this, see [`CockroachStarterBuilder`].
 */
#[derive(Debug)]
pub struct CockroachStarter {
    /// temporary directory used for URL file and potentially data storage
    temp_dir: TempDir,
    /// path to listen URL file (inside temp_dir)
    listen_url_file: PathBuf,
    /// command-line arguments, mirrored here for reporting to the user
    args: Vec<String>,
    /// the command line that we're going to execute
    cmd_builder: tokio::process::Command,
    /// how long to wait for the listen URL to be written
    start_timeout: Duration,
}

impl CockroachStarter {
    /** Returns a human-readable summary of the command line to be executed */
    pub fn cmdline(&self) -> impl fmt::Display {
        self.args.join(" ")
    }

    /**
     * Returns the path to the temporary directory created for this execution
     */
    pub fn temp_dir(&self) -> &Path {
        self.temp_dir.path()
    }

    /**
     * Spawns a new process to run the configured command
     *
     * This function waits up to a fixed timeout for CockroachDB to report its
     * listening URL.  This function fails if the child process exits before
     * that happens or if the timeout expires.
     */
    pub async fn start(
        mut self,
    ) -> Result<CockroachInstance, CockroachStartError> {
        let mut child_process = self.cmd_builder.spawn().map_err(|source| {
            CockroachStartError::BadCmd { cmd: self.args[0].clone(), source }
        })?;
        let pid = child_process.id().unwrap();

        /*
         * Wait for CockroachDB to write out its URL information.  There's not a
         * great way for us to know when this has happened, unfortunately.  So
         * we just poll for it up to some maximum timeout.
         */
        let wait_result = poll::wait_for_condition(
            || {
                /*
                 * If CockroachDB is not running at any point in this process,
                 * stop waiting for the file to become available.
                 * TODO-cleanup This nastiness is because we cannot allow the
                 * mutable reference to "child_process" to be part of the async
                 * block.  However, we need the return value to be part of the
                 * async block.  So we do the process_exited() bit outside the
                 * async block.  We need to move "exited" into the async block,
                 * which means anything we reference gets moved into that block,
                 * which means we need a clone of listen_url_file to avoid
                 * referencing "self".
                 */
                let exited = process_exited(&mut child_process);
                let listen_url_file = self.listen_url_file.clone();
                async move {
                    if exited {
                        return Err(poll::CondCheckError::Failed(
                            CockroachStartError::Exited,
                        ));
                    }

                    /*
                     * When ready, CockroachDB will write the URL on which it's
                     * listening to the specified file.  Try to read this file.
                     * Note that its write is not necessarily atomic, so we wait
                     * for a newline before assuming that it's complete.
                     * TODO-robustness It would be nice if there were a version
                     * of tokio::fs::read_to_string() that accepted a maximum
                     * byte count so that this couldn't, say, use up all of
                     * memory.
                     */
                    match tokio::fs::read_to_string(&listen_url_file).await {
                        Ok(listen_url) if listen_url.contains('\n') => {
                            let listen_url = listen_url.trim_end();
                            let pg_config: tokio_postgres::config::Config =
                                listen_url.parse().map_err(|source| {
                                    poll::CondCheckError::Failed(
                                        CockroachStartError::BadListenUrl {
                                            listen_url: listen_url.to_string(),
                                            source,
                                        },
                                    )
                                })?;
                            Ok((listen_url.to_string(), pg_config))
                        }

                        _ => Err(poll::CondCheckError::NotYet),
                    }
                }
            },
            &Duration::from_millis(10),
            &self.start_timeout,
        )
        .await;

        match wait_result {
            Ok((listen_url, pg_config)) => Ok(CockroachInstance {
                pid,
                listen_url,
                pg_config,
                temp_dir_path: self.temp_dir.path().to_owned(),
                temp_dir: Some(self.temp_dir),
                child_process: Some(child_process),
            }),
            Err(poll::Error::PermanentError(e)) => Err(e),
            Err(poll::Error::TimedOut(time_waited)) => {
                /*
                 * Abort and tell the user.  We'll leave CockroachDB running so
                 * the user can debug if they want.  We'll skip cleanup of the
                 * temporary directory for the same reason and also so that
                 * CockroachDB doesn't trip over its files being gone.
                 */
                self.temp_dir.into_path();
                Err(CockroachStartError::TimedOut { pid, time_waited })
            }
        }
    }
}

#[derive(Debug, Error)]
pub enum CockroachStartError {
    #[error("running {cmd:?} (is the binary installed and on your PATH?)")]
    BadCmd {
        cmd: String,
        #[source]
        source: std::io::Error,
    },

    #[error("cockroach failed to start (see error output above)")]
    Exited,

    #[error("error parsing listen URL {listen_url:?}")]
    BadListenUrl {
        listen_url: String,
        #[source]
        source: tokio_postgres::Error,
    },

    #[error(
        "cockroach did not report listen URL after {time_waited:?} \
        (may still be running as pid {pid} and temporary directory \
        may still exist)"
    )]
    TimedOut { pid: u32, time_waited: Duration },
}

/**
 * Manages a CockroachDB process running as a single-node cluster
 *
 * You are **required** to invoke [`CockroachInstance::wait_for_shutdown()`] or
 * [`CockroachInstance::cleanup()`] before this object is dropped.
 */
#[derive(Debug)]
pub struct CockroachInstance {
    /// child process id
    pid: u32,
    /// Raw listen URL provided by CockroachDB
    listen_url: String,
    /// PostgreSQL config to use to connect to CockroachDB as a SQL client
    pg_config: tokio_postgres::config::Config,
    /// handle to child process, if it hasn't been cleaned up already
    child_process: Option<tokio::process::Child>,
    /// handle to temporary directory, if it hasn't been cleaned up already
    temp_dir: Option<TempDir>,
    /// path to temporary directory
    temp_dir_path: PathBuf,
}

impl CockroachInstance {
    /** Returns the pid of the child process running CockroachDB */
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /**
     * Returns a printable form of the PostgreSQL config provided by
     * CockroachDB
     *
     * This is intended only for printing out.  To actually connect to
     * PostgreSQL, use [`CockroachInstance::pg_config()`].  (Ideally, that
     * object would impl a to_url() or the like, but it does not appear to.)
     */
    pub fn listen_url(&self) -> impl fmt::Display + '_ {
        &self.listen_url
    }

    /**
     * Returns PostgreSQL client configuration suitable for connecting to the
     * CockroachDB database
     */
    pub fn pg_config(&self) -> &tokio_postgres::config::Config {
        &self.pg_config
    }

    /**
     * Returns the path to the temporary directory created for this execution
     */
    pub fn temp_dir(&self) -> &Path {
        &self.temp_dir_path
    }

    /** Returns a connection to the underlying database */
    pub async fn connect(&self) -> Result<Client, tokio_postgres::Error> {
        Client::connect(self.pg_config(), tokio_postgres::NoTls).await
    }

    /** Wrapper around [`wipe()`] using a connection to this database. */
    pub async fn wipe(&self) -> Result<(), anyhow::Error> {
        let client = self.connect().await.context("connect")?;
        wipe(&client).await.context("wipe")?;
        client.cleanup().await.context("cleaning up after wipe")
    }

    /** Wrapper around [`populate()`] using a connection to this database. */
    pub async fn populate(&self) -> Result<(), anyhow::Error> {
        let client = self.connect().await.context("connect")?;
        populate(&client).await.context("populate")?;
        client.cleanup().await.context("cleaning up after wipe")
    }

    /**
     * Waits for the child process to exit
     *
     * Note that CockroachDB will normally run forever unless the caller
     * arranges for it to be shutdown.
     */
    pub async fn wait_for_shutdown(&mut self) -> Result<(), anyhow::Error> {
        /* We do not care about the exit status of this process. */
        #[allow(unused_must_use)]
        {
            self.child_process
                .as_mut()
                .expect("cannot call wait_for_shutdown() after cleanup()")
                .wait()
                .await;
        }
        self.child_process = None;
        self.cleanup().await
    }

    /**
     * Cleans up the child process and temporary directory
     *
     * If the child process is still running, it will be killed with SIGKILL and
     * this function will wait for it to exit.  Then the temporary directory
     * will be cleaned up.
     */
    pub async fn cleanup(&mut self) -> Result<(), anyhow::Error> {
        /*
         * Kill the process and wait for it to exit so that we can remove the
         * temporary directory that we may have used to store its data.  We
         * don't care what the result of the process was.
         */
        if let Some(child_process) = self.child_process.as_mut() {
            child_process
                .start_kill()
                .context("sending SIGKILL to child process")?;
            child_process.wait().await.context("waiting for child process")?;
            self.child_process = None;
        }

        if let Some(temp_dir) = self.temp_dir.take() {
            temp_dir.close().context("cleaning up temporary directory")?;
        }

        Ok(())
    }
}

impl Drop for CockroachInstance {
    fn drop(&mut self) {
        /*
         * TODO-cleanup Ideally at this point we would run self.cleanup() to
         * kill the child process, wait for it to exit, and then clean up the
         * temporary directory.  However, we don't have an executor here with
         * which to run async/await code.  We could create one here, but it's
         * not clear how safe or sketchy that would be.  Instead, we expect that
         * the caller has done the cleanup already.  This won't always happen,
         * particularly for ungraceful failures.
         */
        if self.child_process.is_some() || self.temp_dir.is_some() {
            eprintln!(
                "WARN: dropped CockroachInstance without cleaning it up first \
                (there may still be a child process running and a \
                temporary directory leaked)"
            );

            /* Still, make a best effort. */
            #[allow(unused_must_use)]
            if let Some(child_process) = self.child_process.as_mut() {
                child_process.start_kill();
            }
            #[allow(unused_must_use)]
            if let Some(temp_dir) = self.temp_dir.take() {
                temp_dir.close();
            }
        }
    }
}

/**
 * Wrapper around tokio::process::Child::try_wait() so that we can unwrap() the
 * result in one place with this explanatory comment.
 *
 * The semantics of that function aren't as clear as we'd like.  The docs say:
 *
 * > If the child has exited, then `Ok(Some(status))` is returned. If the
 * > exit status is not available at this time then `Ok(None)` is returned.
 * > If an error occurs, then that error is returned.
 *
 * It seems we can infer that "the exit status is not available at this time"
 * means that the process has not exited.  After all, if it _had_ exited, we'd
 * fall into the first case.  It's not clear under what conditions this function
 * could ever fail.  It's not clear from the source that it's even possible.
 */
fn process_exited(child_process: &mut tokio::process::Child) -> bool {
    child_process.try_wait().unwrap().is_some()
}

/**
 * Populate a database with the Omicron schema and any initial objects
 *
 * This is not idempotent.  It will fail if the database or other objects
 * already exist.
 */
pub async fn populate(
    client: &tokio_postgres::Client,
) -> Result<(), anyhow::Error> {
    let sql = include_str!("../sql/dbinit.sql");
    client.batch_execute(sql).await.context("populating Omicron database")
}

/**
 * Wipe an Omicron database from the remote database
 *
 * This is dangerous!  Use carefully.
 */
pub async fn wipe(
    client: &tokio_postgres::Client,
) -> Result<(), anyhow::Error> {
    let sql = include_str!("../sql/dbwipe.sql");
    client.batch_execute(sql).await.context("wiping Omicron database")
}

/**
 * Returns true if the database that this client is connected to contains
 * the Omicron schema
 *
 * Note that this will change the currently-selected database if the Omicron
 * one exists.
 *
 * Panics if the attempt to run a query fails for any reason other than the
 * schema not existing.  (This is intended to be run from the test suite.)
 */
pub async fn has_omicron_schema(client: &tokio_postgres::Client) -> bool {
    match client.batch_execute("USE omicron; SELECT id FROM Project").await {
        Ok(_) => true,
        Err(e) => {
            let sql_error =
                e.code().expect("got non-SQL error checking for schema");
            assert_eq!(
                *sql_error,
                tokio_postgres::error::SqlState::UNDEFINED_DATABASE
            );
            false
        }
    }
}

/**
 * Wraps a PostgreSQL connection and client as provided by
 * `tokio_postgres::Config::connect()`
 *
 * Typically, callers of [`tokio_postgres::Config::connect()`] get back both a
 * Client and a Connection.  You must spawn a separate task to `await` on the
 * connection in order for any database operations to happen.  When the Client
 * is dropped, the Connection is gracefully terminated, its Future completes,
 * and the task should be cleaned up.  This is awkward to use, particularly if
 * you care to be sure that the task finished.
 *
 * This structure combines the Connection and Client.  You can create one from a
 * [`tokio_postgres::Config`] or from an existing ([`tokio_postgres::Client`],
 * [`tokio_postgres::Connection`]) pair.  You can use it just like a
 * `tokio_postgres::Client`.  When finished, you can call `cleanup()` to drop
 * the Client and wait for the Connection's task.
 *
 * If you do not call `cleanup()`, then the underlying `tokio_postgres::Client`
 * will be dropped when this object is dropped.  If there has been no connection
 * error, then the connection will be closed gracefully, but nothing will check
 * for any error from the connection.
 */
pub struct Client {
    client: tokio_postgres::Client,
    conn_task: tokio::task::JoinHandle<Result<(), tokio_postgres::Error>>,
}

type ClientConnPair<S, T> =
    (tokio_postgres::Client, tokio_postgres::Connection<S, T>);
impl<S, T> From<ClientConnPair<S, T>> for Client
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    fn from((client, connection): ClientConnPair<S, T>) -> Self {
        let join_handle = tokio::spawn(async move { connection.await });
        Client { client, conn_task: join_handle }
    }
}

impl Deref for Client {
    type Target = tokio_postgres::Client;
    fn deref(&self) -> &Self::Target {
        &self.client
    }
}

impl Client {
    /**
     * Invokes `config.connect(tls)` and wraps the result in a `Client`.
     */
    pub async fn connect<T>(
        config: &tokio_postgres::config::Config,
        tls: T,
    ) -> Result<Client, tokio_postgres::Error>
    where
        T: tokio_postgres::tls::MakeTlsConnect<tokio_postgres::Socket>,
        T::Stream: Send + 'static,
    {
        Ok(Client::from(config.connect(tls).await?))
    }

    /**
     * Closes the connection, waits for it to be cleaned up gracefully, and
     * returns any error status.
     */
    pub async fn cleanup(self) -> Result<(), tokio_postgres::Error> {
        drop(self.client);
        self.conn_task.await.expect("failed to join on connection task")
    }
}

/*
 * These are more integration tests than unit tests.
 */
#[cfg(test)]
mod test {
    use super::has_omicron_schema;
    use super::CockroachStartError;
    use super::CockroachStarter;
    use super::CockroachStarterBuilder;
    use crate::dev::poll;
    use crate::dev::process_running;
    use std::env;
    use std::path::Path;
    use std::time::Duration;
    use tempfile::tempdir;
    use tokio::fs;

    fn new_builder() -> CockroachStarterBuilder {
        /*
         * We redirect stdout and stderr to files within the temporary directory
         * so that people don't get reams of irrelevant output when running
         * `cargo test`.  This will be cleaned up as usual on success.
         */
        let mut builder = CockroachStarterBuilder::new();
        builder.redirect_stdio_to_files();
        builder
    }

    /*
     * Tests that we clean up the temporary directory correctly when the starter
     * goes out of scope, even if we never started the instance.  This is
     * important to avoid leaking the directory if there's an error starting the
     * instance, for example.
     */
    #[tokio::test]
    async fn test_starter_tmpdir() {
        let builder = new_builder();
        let starter = builder.build().unwrap();
        let directory = starter.temp_dir().to_owned();
        assert!(fs::metadata(&directory)
            .await
            .expect("temporary directory is missing")
            .is_dir());
        drop(starter);
        assert_eq!(
            libc::ENOENT,
            fs::metadata(&directory)
                .await
                .expect_err("temporary directory still exists")
                .raw_os_error()
                .unwrap()
        );
    }

    /*
     * Tests what happens if the "cockroach" command cannot be found.
     */
    #[tokio::test]
    async fn test_bad_cmd() {
        let builder = CockroachStarterBuilder::new_with_cmd("/nonexistent");
        test_database_start_failure(builder).await;
    }

    /*
     * Tests what happens if the "cockroach" command exits before writing the
     * listening-url file.  This looks the same to the caller (us), but
     * internally requires different code paths.
     */
    #[tokio::test]
    async fn test_cmd_fails() {
        let mut builder = new_builder();
        builder.arg("not-a-valid-argument");
        test_database_start_failure(builder).await;
    }

    /*
     * Helper function for testing cases where the database fails to start.
     */
    async fn test_database_start_failure(builder: CockroachStarterBuilder) {
        let starter = builder.build().unwrap();
        let temp_dir = starter.temp_dir().to_owned();
        eprintln!("will run: {}", starter.cmdline());
        let error =
            starter.start().await.expect_err("unexpectedly started database");
        eprintln!("error: {:?}", error);
        assert_eq!(
            libc::ENOENT,
            fs::metadata(temp_dir)
                .await
                .expect_err("temporary directory still exists")
                .raw_os_error()
                .unwrap()
        );
    }

    /*
     * Tests when CockroachDB hangs on startup by setting the start timeout
     * absurdly short.  This unfortunately doesn't cover all cases.  By choosing
     * a zero timeout, we're not letting the database get very far in its
     * startup.  But we at least ensure that the test suite does not hang or
     * timeout at some very long value.
     */
    #[tokio::test]
    async fn test_database_start_hang() {
        let mut builder = new_builder();
        builder.start_timeout(&Duration::from_millis(0));
        let starter = builder.build().expect("failed to build starter");
        let directory = starter.temp_dir().to_owned();
        eprintln!("temporary directory: {}", directory.display());
        let error =
            starter.start().await.expect_err("unexpectedly started database");
        eprintln!("(expected) error starting database: {:?}", error);
        let pid = match error {
            CockroachStartError::TimedOut { pid, time_waited } => {
                /* We ought to fire a 0-second timeout within 5 seconds. */
                assert!(time_waited < Duration::from_secs(5));
                pid
            }
            other_error => panic!(
                "expected timeout error, but got some other error: {:?}",
                other_error
            ),
        };
        /* The child process should still be running. */
        assert!(process_running(pid));
        /* The temporary directory should still exist. */
        assert!(fs::metadata(&directory)
            .await
            .expect("temporary directory is missing")
            .is_dir());
        /* Kill the child process (to clean up after ourselves). */
        assert_eq!(0, unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) });

        /*
         * Wait for the process to exit so that we can reliably clean up the
         * temporary directory.  We don't have a great way to avoid polling
         * here.
         */
        poll::wait_for_condition::<(), std::convert::Infallible, _, _>(
            || async {
                if process_running(pid) {
                    Err(poll::CondCheckError::NotYet)
                } else {
                    Ok(())
                }
            },
            &Duration::from_millis(25),
            &Duration::from_secs(10),
        )
        .await
        .unwrap_or_else(|_| {
            panic!(
                "timed out waiting for pid {} to exit \
                    (leaving temporary directory {})",
                pid,
                directory.display()
            );
        });
        assert!(!process_running(pid));

        /*
         * The temporary directory is normally cleaned up automatically.  In
         * this case, it's deliberately left around.  We need to clean it up
         * here.  Now, the directory is created with tempfile::TempDir, which
         * puts it under std::env::temp_dir().  We assert that here as an
         * ultra-conservative safety.  We don't want to accidentally try to blow
         * away some large directory tree if somebody modifies the code to use
         * some other directory for the temporary directory.
         */
        if !directory.starts_with(env::temp_dir()) {
            panic!(
                "refusing to remove temporary directory not under
                std::env::temp_dir(): {}",
                directory.display()
            )
        }

        fs::remove_dir_all(&directory).await.unwrap_or_else(|e| {
            panic!(
                "failed to remove temporary directory {}: {:?}",
                directory.display(),
                e
            )
        });
    }

    /*
     * Test the happy path using the default store directory.
     */
    #[tokio::test]
    async fn test_setup_database_default_dir() {
        let starter = new_builder().build().unwrap();

        /*
         * In this configuration, the database directory should exist within the
         * starter's temporary directory.
         */
        let data_dir = starter.temp_dir().join("data");

        /*
         * This common function will verify that the entire temporary directory
         * is cleaned up.  We do not need to check that again here.
         */
        test_setup_database(starter, &data_dir, true).await;
    }

    /*
     * Test the happy path using an overridden store directory.
     */
    #[tokio::test]
    async fn test_setup_database_overridden_dir() {
        let extra_temp_dir =
            tempdir().expect("failed to create temporary directory");
        let data_dir = extra_temp_dir.path().join("custom_data");
        let starter = new_builder().store_dir(&data_dir).build().unwrap();

        /*
         * This common function will verify that the entire temporary directory
         * is cleaned up.  We do not need to check that again here.
         */
        test_setup_database(starter, &data_dir, false).await;

        /*
         * At this point, our extra temporary directory should still exist.
         * This is important -- the library should not clean up a data directory
         * that was specified by the user.
         */
        assert!(fs::metadata(&data_dir)
            .await
            .expect("CockroachDB data directory is missing")
            .is_dir());
        /* Clean it up. */
        let extra_temp_dir_path = extra_temp_dir.path().to_owned();
        extra_temp_dir
            .close()
            .expect("failed to remove extra temporary directory");
        assert_eq!(
            libc::ENOENT,
            fs::metadata(&extra_temp_dir_path)
                .await
                .expect_err("extra temporary directory still exists")
                .raw_os_error()
                .unwrap()
        );
    }

    /*
     * Test the happy path: start the database, run a query against the URL we
     * found, and then shut it down cleanly.
     */
    async fn test_setup_database<P: AsRef<Path>>(
        starter: CockroachStarter,
        data_dir: P,
        test_populate: bool,
    ) {
        /* Start the database. */
        eprintln!("will run: {}", starter.cmdline());
        let mut database =
            starter.start().await.expect("failed to start database");
        let pid = database.pid();
        let temp_dir = database.temp_dir().to_owned();
        eprintln!(
            "database pid {} listening at: {}",
            pid,
            database.listen_url()
        );

        /*
         * The database process should be running and the database's store
         * directory should exist.
         */
        assert!(process_running(pid));
        assert!(fs::metadata(data_dir.as_ref())
            .await
            .expect("CockroachDB data directory is missing")
            .is_dir());

        /* Try to connect to it and run a query. */
        eprintln!("connecting to database");
        let client = database
            .connect()
            .await
            .expect("failed to connect to newly-started database");

        let row = client
            .query_one("SELECT 12345", &[])
            .await
            .expect("basic query failed");
        assert_eq!(row.len(), 1);
        assert_eq!(row.get::<'_, _, i64>(0), 12345);

        /*
         * Run some tests using populate() and wipe().
         */
        if test_populate {
            assert!(!has_omicron_schema(&client).await);
            eprintln!("populating database (1)");
            database.populate().await.expect("populating database (1)");
            assert!(has_omicron_schema(&client).await);
            /*
             * populate() fails if the database is already populated.  We don't
             * want to accidentally destroy data by wiping it first
             * automatically.
             */
            database.populate().await.expect_err("populated database twice");
            eprintln!("wiping database (1)");
            database.wipe().await.expect("wiping database (1)");
            assert!(!has_omicron_schema(&client).await);
            /* On the other hand, wipe() is idempotent. */
            database.wipe().await.expect("wiping database (2)");
            assert!(!has_omicron_schema(&client).await);
            /* populate() should work again after a wipe(). */
            eprintln!("populating database (2)");
            database.populate().await.expect("populating database (2)");
            assert!(has_omicron_schema(&client).await);
        }

        client.cleanup().await.expect("connection unexpectedly failed");
        database.cleanup().await.expect("failed to clean up database");

        /* Check that the database process is no longer running. */
        assert!(!process_running(pid));

        /*
         * Check that the temporary directory used by the starter has been
         * cleaned up.
         */
        assert_eq!(
            libc::ENOENT,
            fs::metadata(&temp_dir)
                .await
                .expect_err("temporary directory still exists")
                .raw_os_error()
                .unwrap()
        );

        eprintln!("cleaned up database and temporary directory");
    }

    /*
     * Test that you can run the database twice concurrently (and have different
     * databases!).
     */
    #[tokio::test]
    async fn test_database_concurrent() {
        let db1 = new_builder()
            .build()
            .expect("failed to create starter for the first database")
            .start()
            .await
            .expect("failed to start first database");
        let db2 = new_builder()
            .build()
            .expect("failed to create starter for the second database")
            .start()
            .await
            .expect("failed to start second database");
        let client1 =
            db1.connect().await.expect("failed to connect to first database");
        let client2 =
            db2.connect().await.expect("failed to connect to second database");

        client1
            .execute("CREATE TABLE foo (v int)", &[])
            .await
            .expect("create (1)");
        client2
            .execute("CREATE TABLE foo (v int)", &[])
            .await
            .expect("create (2)");
        client1
            .execute("INSERT INTO foo VALUES (5)", &[])
            .await
            .expect("insert");
        client1.cleanup().await.expect("first connection closed ungracefully");

        let rows =
            client2.query("SELECT v FROM foo", &[]).await.expect("list rows");
        assert_eq!(rows.len(), 0);
        client2.cleanup().await.expect("second connection closed ungracefully");
    }
}
