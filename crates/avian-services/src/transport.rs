use std::{
    net::{IpAddr, SocketAddr},
    time::Duration,
};

use avian_membership::MembershipStore;
use reqwest::{Client, Url};

use crate::{AnnouncementPage, Limits, Result, ServiceError, active_member};

/// The overlay must authenticate transport possession before this address check.
/// A relay need not own the announcements it transports; every owner signature is checked separately.
pub fn verify_overlay_peer(
    membership: &MembershipStore,
    peer_node_id: &str,
    address: IpAddr,
) -> Result<()> {
    let private = match address {
        IpAddr::V4(address) => address.is_private(),
        IpAddr::V6(address) => address.segments()[0] & 0xfe00 == 0xfc00,
    };
    if !private
        || !active_member(membership, peer_node_id)?
            .overlay_addresses
            .contains(&address)
    {
        return Err(ServiceError::Unauthorized);
    }
    Ok(())
}

pub fn synchronization_client() -> Result<Client> {
    Ok(Client::builder()
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(5))
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .build()?)
}

#[derive(Clone, Debug)]
pub struct FetchRequest<'a> {
    pub peer_node_id: &'a str,
    pub endpoint: SocketAddr,
    pub after: Option<&'a str>,
    pub limit: usize,
    pub now_unix: i64,
}

pub async fn fetch_announcements(
    client: &Client,
    membership: &MembershipStore,
    request: FetchRequest<'_>,
    limits: &Limits,
) -> Result<AnnouncementPage> {
    let FetchRequest {
        peer_node_id,
        endpoint,
        after,
        limit,
        now_unix,
    } = request;
    limits.validate()?;
    verify_overlay_peer(membership, peer_node_id, endpoint.ip())?;
    if endpoint.port() == 0 || limit == 0 || limit > limits.maximum_page_size {
        return Err(ServiceError::InvalidField("peer endpoint or page size"));
    }
    if after.is_some_and(|cursor| cursor.len() != 64 || hex::decode(cursor).is_err()) {
        return Err(ServiceError::InvalidField("page cursor"));
    }
    let mut url = Url::parse(&format!("http://{endpoint}/v1/service-announcements"))
        .map_err(|_| ServiceError::InvalidField("peer endpoint"))?;
    {
        let mut query = url.query_pairs_mut();
        query.append_pair("limit", &limit.to_string());
        if let Some(cursor) = after {
            query.append_pair("after", cursor);
        }
    }
    let page = bounded_fetch(client, url, limits.maximum_registry_bytes).await?;
    verify_page(&page, membership, after, limit, now_unix, limits)?;
    Ok(page)
}

async fn bounded_fetch(client: &Client, url: Url, budget: usize) -> Result<AnnouncementPage> {
    let mut response = client.get(url).send().await?.error_for_status()?;
    if response
        .content_length()
        .is_some_and(|length| length > budget as u64)
    {
        return Err(ServiceError::ResourceBudget);
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if body.len().saturating_add(chunk.len()) > budget {
            return Err(ServiceError::ResourceBudget);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(serde_json::from_slice(&body)?)
}

fn verify_page(
    page: &AnnouncementPage,
    membership: &MembershipStore,
    after: Option<&str>,
    limit: usize,
    now_unix: i64,
    limits: &Limits,
) -> Result<()> {
    if page.version != 1 || page.network_id != membership.trust_root().network_id {
        return Err(ServiceError::WrongNetwork);
    }
    if page.records.len() > limit {
        return Err(ServiceError::ResourceBudget);
    }
    let mut previous = after;
    for record in &page.records {
        record.verify(membership, now_unix, limits)?;
        let node_id = record.claims.owner_node_id.as_str();
        if previous.is_some_and(|cursor| node_id <= cursor) {
            return Err(ServiceError::Rollback);
        }
        previous = Some(node_id);
    }
    if let Some(cursor) = page.next_cursor.as_deref()
        && page
            .records
            .last()
            .is_none_or(|record| record.claims.owner_node_id != cursor)
    {
        return Err(ServiceError::InvalidField("next cursor"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn http_reader_enforces_declared_and_streamed_budgets() {
        for declared in [true, false] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let (mut connection, _) = listener.accept().await.unwrap();
                let mut request = [0u8; 2048];
                let count = connection.read(&mut request).await.unwrap();
                assert!(count > 0);
                let header = if declared {
                    "Content-Length: 256\r\n"
                } else {
                    ""
                };
                let reply = format!(
                    "HTTP/1.1 200 OK\r\n{header}Connection: close\r\n\r\n{}",
                    "x".repeat(256)
                );
                let _ = connection.write_all(reply.as_bytes()).await;
            });
            let result = bounded_fetch(
                &synchronization_client().unwrap(),
                Url::parse(&format!("http://{address}/")).unwrap(),
                128,
            )
            .await;
            assert!(matches!(result, Err(ServiceError::ResourceBudget)));
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn loopback_http_page_retains_signed_owner_verification() {
        use crate::{ServiceEndpoint, ServiceKind, SignedAnnouncement, TransportProtocol};
        use avian_membership::{MembershipClaims, Permissions, TrustRoot, issue_membership};
        use ed25519_dalek::SigningKey;

        let administrator = SigningKey::from_bytes(&[60; 32]);
        let owner = SigningKey::from_bytes(&[61; 32]);
        let root = TrustRoot::new("http-service-test", &administrator.verifying_key()).unwrap();
        let mut membership = MembershipStore::new(root.clone()).unwrap();
        let address = "10.92.0.1".parse().unwrap();
        let claims = MembershipClaims::new(
            "http-service-test",
            &owner.verifying_key(),
            [62; 32],
            vec![address],
            1,
            Permissions::default(),
        );
        membership
            .import_membership(issue_membership(&root, &administrator, claims).unwrap())
            .unwrap();
        let limits = Limits::default();
        let record = SignedAnnouncement::sign(
            &membership,
            &owner,
            1,
            vec![ServiceEndpoint {
                kind: ServiceKind::ArcControl,
                address,
                protocol: TransportProtocol::Tcp,
                port: 5760,
            }],
            60,
            1000,
            &limits,
        )
        .unwrap();
        let page = AnnouncementPage {
            version: 1,
            network_id: "http-service-test".into(),
            generated_at_unix: 1000,
            records: vec![record],
            next_cursor: None,
        };
        for tampered in [false, true] {
            let mut response = page.clone();
            if tampered {
                response.records[0].claims.endpoints[0].port += 1;
            }
            let body = serde_json::to_vec(&response).unwrap();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let endpoint = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let (mut connection, _) = listener.accept().await.unwrap();
                let mut request = [0u8; 2048];
                assert!(connection.read(&mut request).await.unwrap() > 0);
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                connection.write_all(header.as_bytes()).await.unwrap();
                connection.write_all(&body).await.unwrap();
            });
            let received = bounded_fetch(
                &synchronization_client().unwrap(),
                Url::parse(&format!("http://{endpoint}/v1/service-announcements")).unwrap(),
                limits.maximum_registry_bytes,
            )
            .await
            .unwrap();
            let verified = verify_page(&received, &membership, None, 1, 1001, &limits);
            if tampered {
                assert!(matches!(verified, Err(ServiceError::InvalidSignature)));
            } else {
                verified.unwrap();
            }
            server.await.unwrap();
        }
    }
}
