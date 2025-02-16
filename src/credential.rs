use ironrdp::{
    connector::sspi::{AuthIdentity, Secret, UserNameFormat, Username},
    server::{CredentialChecker, Credentials},
};

pub struct DummyCredential;

impl CredentialChecker for DummyCredential {
    fn auth_data(&self, _username: &str) -> Option<AuthIdentity> {
        Some(AuthIdentity {
            username: Username::new("user", None).ok()?,
            password: Secret::new("user".to_string()),
        })
    }

    fn check(&self, credential: &Credentials) -> bool {
        credential.username == "user" && credential.password == "user"
    }
}
