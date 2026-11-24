use std::collections::VecDeque;
use std::ops::{ControlFlow, Deref};
use std::path::Path;

use libredox::error::Error as LError;
use redox_scheme::scheme::{SchemeState, SchemeSync};
use redox_scheme::{Request, RequestKind, Response, SignalBehavior, Socket};
use syscall::Result;
use syscall::error::{self as errno, Error};

const MAX_SCHEME_READ_EINTR_RETRIES: usize = 64;

fn program_name() -> String {
    std::env::args()
        .next()
        .and_then(|arg| {
            Path::new(&arg)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| String::from("scheme-utils"))
}

pub struct Blocking<S> {
    // TODO: VecDeque for both when it implements spare_capacity
    requests_read: Vec<Request>,
    responses_to_write: VecDeque<Response>,

    // FIXME only allow Socket once netstack no longer needs this to be a borrow
    socket: S,
    state: SchemeState,
}

impl<S: Deref<Target = Socket>> Blocking<S> {
    pub fn new(socket: S, queue_size: usize) -> Self {
        Self {
            requests_read: Vec::with_capacity(queue_size),
            responses_to_write: VecDeque::with_capacity(queue_size),
            socket,
            state: SchemeState::new(),
        }
    }

    pub fn socket(&self) -> &Socket {
        &self.socket
    }

    pub fn process_requests_nonblocking(
        &mut self,
        scheme: &mut impl SchemeSync,
    ) -> Result<ControlFlow<()>> {
        self.process_requests_inner(scheme, false)
    }

    fn process_requests_inner(
        &mut self,
        scheme: &mut impl SchemeSync,
        retry_eintr: bool,
    ) -> Result<ControlFlow<()>> {
        assert!(self.requests_read.is_empty());
        assert!(self.responses_to_write.is_empty());

        let mut eintr_retries: usize = 0;
        loop {
            match self
                .socket
                .read_requests(&mut self.requests_read, SignalBehavior::Interrupt)
            {
                Ok(()) if self.requests_read.is_empty() => {
                    unreachable!("blocking scheme read failed to read anything");
                }
                Ok(()) => break,
                Err(Error {
                    errno: errno::EINTR,
                }) if retry_eintr => {
                    eintr_retries += 1;
                    let name = program_name();
                    eprintln!("{name}: blocking scheme wait interrupted; retrying");
                    if eintr_retries >= MAX_SCHEME_READ_EINTR_RETRIES {
                        panic!(
                            "{name}: blocking scheme wait exceeded MAX_SCHEME_READ_EINTR_RETRIES ({MAX_SCHEME_READ_EINTR_RETRIES})"
                        );
                    }
                }
                Err(Error {
                    errno: errno::EINTR | errno::EWOULDBLOCK | errno::EAGAIN,
                }) => return Ok(ControlFlow::Break(())),
                Err(err) => return Err(err),
            }
        }

        for request in self.requests_read.drain(..) {
            match request.kind() {
                RequestKind::Call(req) => {
                    let response = req.handle_sync(scheme, &mut self.state);
                    self.responses_to_write.push_back(response);
                }
                RequestKind::Cancellation(_req) => {}
                RequestKind::OnClose { id } => {
                    // TODO: state.on_close()
                    scheme.on_close(id);
                }
                RequestKind::SendFd(sendfd_request) => {
                    let result = scheme.on_sendfd(&sendfd_request);
                    let response = Response::new(result, sendfd_request);
                    self.responses_to_write.push_back(response);
                }
                RequestKind::RecvFd(recvfd_request) => {
                    let result = scheme.on_recvfd(&recvfd_request);
                    let response = Response::open_dup_like(result, recvfd_request);
                    self.responses_to_write.push_back(response);
                }
                RequestKind::MsyncMsg | RequestKind::MunmapMsg | RequestKind::MmapMsg => {
                    unreachable!()
                }
                RequestKind::OnDetach { .. } => {}
            }
        }

        match self
            .socket
            .write_responses(&mut self.responses_to_write, SignalBehavior::Restart)
        {
            Ok(()) if !self.responses_to_write.is_empty() => {
                panic!("failed to write all scheme responses");
            }
            Ok(()) => Ok(ControlFlow::Continue(())),
            Err(Error {
                errno: errno::EINTR | errno::EWOULDBLOCK | errno::EAGAIN,
            }) => {
                panic!("scheme response writing should always block");
            }
            Err(err) => Err(LError::from(err).into()),
        }
    }

    pub fn process_requests_blocking(mut self, mut scheme: impl SchemeSync) -> Result<!> {
        loop {
            match self.process_requests_inner(&mut scheme, true)? {
                ControlFlow::Continue(()) => {}
                ControlFlow::Break(()) => {
                    panic!("process_requests_blocking should not be used on non-blocking schemes");
                }
            }
        }
    }
}
