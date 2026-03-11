use deadpool::managed::{self, Metrics};
use ldap3::{drive, exop::WhoAmI, Ldap, LdapConnAsync, LdapError, Scope, SearchEntry};
use rand::prelude::SliceRandom;
use std::sync::Arc;
use trust_dns_resolver::{
    config::{ResolverConfig, ResolverOpts},
    TokioAsyncResolver,
};

type Pool = managed::Pool<LdapManager>;

#[derive(Clone)]
pub struct LdapClient {
    ldap: Arc<Pool>,
}

#[derive(Clone)]
pub struct LdapManager {
    ldap_servers: Vec<String>,
    bind_dn: String,
    bind_pw: String,
}

impl LdapManager {
    pub async fn new(bind_dn: &str, bind_pw: &str) -> Self {
        let ldap_servers = get_ldap_servers().await;
        Self {
            ldap_servers,
            bind_dn: bind_dn.to_string(),
            bind_pw: bind_pw.to_string(),
        }
    }
}

#[async_trait::async_trait]
impl managed::Manager for LdapManager {
    type Type = Ldap;
    type Error = LdapError;

    async fn create(&self) -> Result<Self::Type, Self::Error> {
        let server = self
            .ldap_servers
            .choose(&mut rand::thread_rng())
            .expect("No LDAP servers found in DNS");

        let (conn, mut ldap) = LdapConnAsync::new(server).await?;
        drive!(conn);
        ldap.simple_bind(&self.bind_dn, &self.bind_pw)
            .await?
            .success()?;

        Ok(ldap)
    }

    async fn recycle(
        &self,
        ldap: &mut Self::Type,
        _: &Metrics,
    ) -> managed::RecycleResult<Self::Error> {
        ldap.extended(WhoAmI).await?.success()?;
        Ok(())
    }
}

impl LdapClient {
    pub async fn new(bind_dn: &str, bind_pw: &str) -> Self {
        let ldap_manager = LdapManager::new(bind_dn, bind_pw).await;
        let ldap_pool = Pool::builder(ldap_manager)
            .max_size(5)
            .build()
            .expect("Failed to build LDAP pool");

        Self {
            ldap: Arc::new(ldap_pool),
        }
    }

    pub async fn get_csh_uid_by_plex(
        &self,
        plex_username: &str,
    ) -> Result<Option<String>, Box<dyn std::error::Error>> {
        let mut ldap = self.ldap.get().await?;

        let safe_plex_username = plex_username
            .replace('\\', "\\5c")
            .replace('*', "\\2a")
            .replace('(', "\\28")
            .replace(')', "\\29")
            .replace('\0', "\\00");

        let search_filter = format!("(plex={})", safe_plex_username);
        let user_ou = "cn=users,cn=accounts,dc=csh,dc=rit,dc=edu";

        let (rs, _res) = ldap
            .search(user_ou, Scope::Subtree, &search_filter, vec!["uid"])
            .await?
            .success()?;

        if let Some(entry) = rs.into_iter().next() {
            let search_entry = SearchEntry::construct(entry);
            if let Some(uid_vals) = search_entry.attrs.get("uid") {
                if let Some(uid) = uid_vals.first() {
                    return Ok(Some(uid.to_string()));
                }
            }
        }

        Ok(None)
    }
}

async fn get_ldap_servers() -> Vec<String> {
    let resolver = TokioAsyncResolver::tokio(ResolverConfig::default(), ResolverOpts::default());

    match resolver.srv_lookup("_ldap._tcp.csh.rit.edu").await {
        Ok(response) => response
            .iter()
            .map(|record| format!("ldaps://{}", record.target().to_string().trim_end_matches('.')))
            .collect(),
        Err(_) => vec![
            "ldaps://ipa10-nrh.csh.rit.edu".to_string(),
            "ldaps://ipa11-tss.csh.rit.edu".to_string(),
        ],
    }
}