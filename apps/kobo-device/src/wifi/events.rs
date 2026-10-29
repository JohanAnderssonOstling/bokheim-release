//! One blocking wait for requests, supplicant messages, address changes and
//! control-socket replacement. No helper thread or steady-state timer.

use super::{Controller, Job};
use std::ffi::CString;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::time::Instant;

pub(super) struct Sender {
    jobs: mpsc::Sender<Job>,
    wake: Arc<OwnedFd>,
}

impl Sender {
    pub(super) fn send(&self, job: Job) -> io::Result<()> {
        self.jobs.send(job).map_err(|_| io::Error::from(io::ErrorKind::BrokenPipe))?;
        let value = 1_u64.to_ne_bytes();
        loop {
            // An already-full counter is already readable, so no wake is lost.
            let result = unsafe { libc::write(self.wake.as_raw_fd(), value.as_ptr().cast(), value.len()) };
            if result >= 0 {
                return Ok(());
            }
            let error = io::Error::last_os_error();
            match error.kind() {
                io::ErrorKind::Interrupted => continue,
                io::ErrorKind::WouldBlock => return Ok(()),
                _ => return Err(error),
            }
        }
    }
}

pub(super) enum Wake {
    Job(Job),
    Refresh { service_changed: bool, monitor_failed: bool, monitor_only: bool },
    Closed,
}

pub(super) struct Events {
    wake: Arc<OwnedFd>,
    route: Option<OwnedFd>,
    service: Option<OwnedFd>,
    socket_path: Option<PathBuf>,
    next_check: Option<Instant>,
}

fn owned_fd(fd: RawFd) -> io::Result<OwnedFd> {
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        // These constructors return a new descriptor owned exclusively here.
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }
}

impl Events {
    pub(super) fn channel(socket_path: Option<&Path>) -> io::Result<(Sender, mpsc::Receiver<Job>, Self)> {
        let wake = Arc::new(owned_fd(unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) })?);
        let (jobs, receive) = mpsc::channel();
        let mut events = Self { wake: wake.clone(), route: None, service: None, socket_path: socket_path.map(Path::to_owned), next_check: None };
        if socket_path.is_some() {
            events.reconnect();
        }
        Ok((Sender { jobs, wake }, receive, events))
    }

    pub(super) fn observe(&mut self, socket_path: Option<&Path>) {
        self.socket_path = socket_path.map(Path::to_owned);
        self.reconnect();
    }

    // Failure leaves the existing recovery timer enabled. Retry when the worker
    // next handles a request/event/deadline, without creating another timer.
    fn reconnect(&mut self) {
        if self.route.is_none() {
            self.route = route_socket().ok();
        }
        if self.service.is_none() {
            self.service = self.socket_path.as_deref().and_then(|path| watch_service(path).ok());
        }
    }

    pub(super) fn available(&self) -> bool {
        self.route.is_some() && self.service.is_some()
    }

    pub(super) fn receive(&mut self, receive: &mpsc::Receiver<Job>, controller: &Controller) -> io::Result<Wake> {
        self.reconnect();
        self.next_check = controller.poll_interval(self.available()).map(|timeout| {
            let next = Instant::now() + timeout;
            self.next_check.map_or(next, |previous| previous.min(next))
        });
        loop {
            // Check the queue before waiting. Enqueue-before-signal also covers
            // a sender racing with poll or with draining the event counter.
            match receive.try_recv() {
                Ok(job) => return Ok(Wake::Job(job)),
                Err(mpsc::TryRecvError::Disconnected) => return Ok(Wake::Closed),
                Err(mpsc::TryRecvError::Empty) => {}
            }
            // Unrelated supplicant chatter must not postpone scan, connection
            // or DHCP checks, even if its descriptor remains continuously ready.
            if self.next_check.is_some_and(|deadline| Instant::now() >= deadline) {
                self.next_check = None;
                return Ok(Wake::Refresh { service_changed: false, monitor_failed: false, monitor_only: false });
            }
            let observing = !controller.sleeping;
            let monitor = observing.then(|| controller.monitor.as_ref().map(|monitor| monitor.fd())).flatten();
            let mut fds = [
                poll_fd(Some(self.wake.as_raw_fd())),
                poll_fd(monitor),
                poll_fd(if observing && controller.radio_enabled { self.route.as_ref().map(AsRawFd::as_raw_fd) } else { None }),
                poll_fd(if observing { self.service.as_ref().map(AsRawFd::as_raw_fd) } else { None }),
            ];
            let timeout = timeout_ms(self.next_check);
            let count = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout) };
            if count < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            if count == 0 {
                self.next_check = None;
                return Ok(Wake::Refresh { service_changed: false, monitor_failed: false, monitor_only: false });
            }
            if fds[0].revents != 0 {
                drain(self.wake.as_raw_fd(), &mut [0; 8])?;
                // Requests (especially suspend) take priority over status work.
                match receive.try_recv() {
                    Ok(job) => return Ok(Wake::Job(job)),
                    Err(mpsc::TryRecvError::Disconnected) => return Ok(Wake::Closed),
                    Err(mpsc::TryRecvError::Empty) => {}
                }
            }
            let mut changed = fds[1].revents != 0;
            let monitor_failed = fds[1].revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0;
            if fds[2].revents != 0 {
                changed = true;
                if fds[2].revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 || drain(fds[2].fd, &mut [0; 16384]).is_err() {
                    self.route = None;
                }
            }
            let mut service_changed = false;
            if fds[3].revents != 0 {
                match self.service_changes() {
                    Ok((relevant, invalidated)) => {
                        service_changed = relevant;
                        if invalidated {
                            self.service = None;
                        }
                    }
                    Err(_) => {
                        self.service = None;
                        service_changed = true;
                    }
                }
                if fds[3].revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
                    self.service = None;
                    service_changed = true;
                }
                changed |= service_changed;
            }
            if changed {
                let monitor_only = fds[1].revents != 0 && !monitor_failed && fds[2].revents == 0 && !service_changed;
                return Ok(Wake::Refresh { service_changed, monitor_failed, monitor_only });
            }
        }
    }

    pub(super) fn service_changes(&self) -> io::Result<(bool, bool)> {
        let mut changed = false;
        let mut invalidated = false;
        let name = self.socket_path.as_deref().and_then(Path::file_name).map(OsStrExt::as_bytes);
        let mut bytes = [0_u8; 4096];
        for _ in 0..128 {
            let count = unsafe { libc::read(self.service.as_ref().unwrap().as_raw_fd(), bytes.as_mut_ptr().cast(), bytes.len()) };
            if count < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                if error.kind() == io::ErrorKind::WouldBlock {
                    break;
                }
                return Err(error);
            }
            if count == 0 {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            let mut offset = 0;
            let header = size_of::<libc::inotify_event>();
            while offset + header <= count as usize {
                // inotify_event has a trailing variable-length filename; the
                // read buffer need not have the struct's alignment.
                let event = unsafe { bytes.as_ptr().add(offset).cast::<libc::inotify_event>().read_unaligned() };
                let end = offset + header + event.len as usize;
                if end > count as usize {
                    return Err(io::ErrorKind::InvalidData.into());
                }
                invalidated |= event.mask & (libc::IN_IGNORED | libc::IN_DELETE_SELF | libc::IN_MOVE_SELF) != 0;
                let filename = &bytes[offset + header..end];
                let filename = filename.split(|byte| *byte == 0).next().unwrap_or_default();
                changed |= invalidated || event.mask & libc::IN_Q_OVERFLOW != 0 || Some(filename) == name;
                offset = end;
            }
        }
        Ok((changed, invalidated))
    }
}

fn poll_fd(fd: Option<RawFd>) -> libc::pollfd {
    libc::pollfd { fd: fd.unwrap_or(-1), events: libc::POLLIN, revents: 0 }
}

fn timeout_ms(deadline: Option<Instant>) -> libc::c_int {
    deadline.map_or(-1, |deadline| {
        let duration = deadline.saturating_duration_since(Instant::now());
        duration.as_millis().saturating_add(u128::from(duration.subsec_nanos() % 1_000_000 != 0)).min(libc::c_int::MAX as u128) as libc::c_int
    })
}

fn drain(fd: RawFd, bytes: &mut [u8]) -> io::Result<()> {
    // Bound each batch so requests cannot be starved by continuous events.
    for _ in 0..128 {
        let count = unsafe { libc::read(fd, bytes.as_mut_ptr().cast(), bytes.len()) };
        if count > 0 {
            continue;
        }
        if count == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        let error = io::Error::last_os_error();
        match error.kind() {
            io::ErrorKind::Interrupted => continue,
            io::ErrorKind::WouldBlock => return Ok(()),
            _ => return Err(error),
        }
    }
    Ok(())
}

fn route_socket() -> io::Result<OwnedFd> {
    let socket = owned_fd(unsafe { libc::socket(libc::AF_NETLINK, libc::SOCK_RAW | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK, libc::NETLINK_ROUTE) })?;
    let mut address: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
    address.nl_family = libc::AF_NETLINK as _;
    address.nl_groups = (libc::RTMGRP_LINK | libc::RTMGRP_IPV4_IFADDR | libc::RTMGRP_IPV6_IFADDR) as _;
    let result = unsafe { libc::bind(socket.as_raw_fd(), (&address as *const libc::sockaddr_nl).cast(), size_of_val(&address) as libc::socklen_t) };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(socket)
}

fn watch_service(path: &Path) -> io::Result<OwnedFd> {
    let parent = path.parent().ok_or(io::ErrorKind::InvalidInput)?;
    let parent = CString::new(parent.as_os_str().as_bytes())?;
    let watcher = owned_fd(unsafe { libc::inotify_init1(libc::IN_CLOEXEC | libc::IN_NONBLOCK) })?;
    let mask = libc::IN_CREATE | libc::IN_DELETE | libc::IN_MOVED_FROM | libc::IN_MOVED_TO | libc::IN_DELETE_SELF | libc::IN_MOVE_SELF;
    if unsafe { libc::inotify_add_watch(watcher.as_raw_fd(), parent.as_ptr(), mask) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(watcher)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wifi::Phase;
    use std::os::unix::net::{UnixDatagram, UnixStream};
    use std::time::Duration;

    #[test]
    fn address_notifications_wake_status_without_waiting_for_a_check() {
        let directory = tempfile::tempdir().unwrap();
        let (_sender, receive, mut events) = Events::channel(Some(&directory.path().join("wlan0"))).unwrap();
        // Substitute the kernel source to exercise its readiness path without
        // requiring root privileges to change a real network interface.
        let (input, output) = UnixDatagram::pair().unwrap();
        input.set_nonblocking(true).unwrap();
        events.route = Some(input.into());
        let mut controller = Controller::new();
        controller.radio_enabled = true;
        controller.snapshot.phase = Phase::Connected;
        output.send(b"address changed").unwrap();
        let started = Instant::now();
        assert!(matches!(events.receive(&receive, &controller).unwrap(), Wake::Refresh { monitor_only: false, service_changed: false, .. }));
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn failed_event_source_is_removed_and_keeps_the_recovery_timer() {
        let directory = tempfile::tempdir().unwrap();
        let (_sender, receive, mut events) = Events::channel(Some(&directory.path().join("wlan0"))).unwrap();
        let (input, output) = UnixStream::pair().unwrap();
        input.set_nonblocking(true).unwrap();
        events.route = Some(input.into());
        let mut controller = Controller::new();
        controller.radio_enabled = true;
        drop(output);
        assert!(matches!(events.receive(&receive, &controller).unwrap(), Wake::Refresh { .. }));
        assert!(!events.available());
        assert_eq!(controller.poll_interval(events.available()), Some(Duration::from_secs(30)));
    }

    #[test]
    fn continuous_events_do_not_postpone_an_existing_check_deadline() {
        let (_sender, receive, mut events) = Events::channel(None).unwrap();
        let (input, output) = UnixDatagram::pair().unwrap();
        input.set_nonblocking(true).unwrap();
        events.route = Some(input.into());
        events.next_check = Some(Instant::now() - Duration::from_secs(1));
        let mut controller = Controller::new();
        controller.radio_enabled = true;
        controller.scan_started = Some(Instant::now());
        output.send(b"unrelated event").unwrap();
        assert!(matches!(events.receive(&receive, &controller).unwrap(), Wake::Refresh { .. }));
        // Servicing the deadline takes priority over the still-readable source.
        let mut bytes = [0; 64];
        assert!(unsafe { libc::read(events.route.as_ref().unwrap().as_raw_fd(), bytes.as_mut_ptr().cast(), bytes.len()) } > 0);
    }
}
