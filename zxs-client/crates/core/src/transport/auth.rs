use std::sync::Arc;

use super::{Body, Builder, Error, Method, Remote, Response, Result, SessionToken, invalid};

type Observer = Arc<dyn Fn() -> Result<()> + Send + Sync>;

pub(super) struct Credentials {
    pub token: Option<SessionToken>,
    password: Option<String>,
    observer: Option<Observer>,
}

impl Credentials {
    pub fn new(token: Option<SessionToken>) -> Self {
        Self {
            token,
            password: None,
            observer: None,
        }
    }
}

impl Remote {
    /// # Errors
    /// Returns an error if the in-memory session cannot be updated.
    pub fn use_password(&self, password: String) -> Result<()> {
        self.auth
            .lock()
            .map_err(|_| invalid("session unavailable"))?
            .password = Some(password);
        Ok(())
    }

    /// Establishes an authenticated session using the configured password.
    ///
    /// # Errors
    /// Returns an error if the server cannot be reached or rejects the password.
    pub fn authenticate(&self) -> Result<()> {
        self.current_token(None).map(|_| ())
    }

    #[must_use]
    pub fn uses_credentials(&self, server: &str, password: &str) -> bool {
        self.endpoint() == server
            && self
                .auth
                .lock()
                .is_ok_and(|auth| auth.password.as_deref() == Some(password))
    }

    pub(crate) fn on_password_rejected(&self, notify: Observer) -> Result<()> {
        self.auth
            .lock()
            .map_err(|_| invalid("session unavailable"))?
            .observer = Some(notify);
        Ok(())
    }

    pub(super) fn token(&self) -> Result<Option<SessionToken>> {
        Ok(self
            .auth
            .lock()
            .map_err(|_| invalid("session unavailable"))?
            .token
            .clone())
    }

    pub(super) fn current_token(&self, rejected: Option<&SessionToken>) -> Result<SessionToken> {
        let mut auth = self
            .auth
            .lock()
            .map_err(|_| invalid("session unavailable"))?;
        if let Some(current) = &auth.token
            && Some(current) != rejected
        {
            return Ok(current.clone());
        }
        let Some(password) = &auth.password else {
            return Err(Error::Remote {
                status: 401,
                message: "请登录服务器".into(),
            });
        };
        match self.login(password) {
            Ok(session) => {
                auth.token = Some(session.token.clone());
                Ok(session.token)
            }
            Err(error @ Error::PasswordRejected(_)) => {
                auth.token = None;
                auth.password = None;
                if let Some(observer) = &auth.observer {
                    observer()?;
                }
                Err(error)
            }
            Err(error) => Err(error),
        }
    }

    pub(super) fn authorized(
        &self,
        method: Method,
        path: &str,
        mut send: impl FnMut(Builder) -> Result<Response<Body>>,
    ) -> Result<Response<Body>> {
        let token = self.current_token(None)?;
        let request = self
            .request(method.clone(), path)
            .header("authorization", format!("Bearer {token}"));
        let response = send(request)?;
        if response.status().as_u16() != 401 {
            return Ok(response);
        }
        drop(response);
        let renewed = self.current_token(Some(&token))?;
        send(
            self.request(method, path)
                .header("authorization", format!("Bearer {renewed}")),
        )
    }
}
