//! La IP del cliente, resuelta UNA vez por request (middleware) y disponible para todos
//! los handlers SIN cambiar sus firmas.
//!
//! Cómo viaja: el middleware la deja en un header INTERNO (`x-ovaultd-ip-cliente`) que
//! **siempre** pisa — si el cliente lo manda, se descarta; si no hay IP resuelta, se borra.
//! Los helpers la leen de ahí (los `HeaderMap` no llevan extensiones, así que el header
//! interno es el canal más simple que no toca ninguna firma).
//!
//! Política (fix de la auditoría 2026-10-18):
//!  - Los headers `X-Real-IP` / `X-Forwarded-For` son de CUALQUIERA que pueda hablarle al
//!    server: solo se obedecen con `trust_proxy` activo (hay un borde de confianza adelante).
//!  - Sin `trust_proxy`, la IP sale del peer TCP real (la del socket), que el cliente no
//!    puede falsear. Si el server no está montado con connect info (tests con `oneshot`),
//!    queda `None` y la clave del freno usa `"-"`.
//!
//! Consecuencia directa: rotar `X-Forwarded-For` ya no esquiva el freno de login.

use axum::{
    extract::{ConnectInfo, Request, State},
    http::{HeaderMap, HeaderName, HeaderValue},
    middleware::Next,
    response::Response,
};
use std::net::{IpAddr, SocketAddr};

use crate::routes::EstadoServidor;

/// Header interno con la IP resuelta. NO es API pública: el middleware lo pisa en cada
/// request (un valor puesto por el cliente nunca sobrevive).
const HEADER_IP: HeaderName = HeaderName::from_static("x-ovaultd-ip-cliente");

/// Middleware: resuelve la IP (headers solo si `trust_proxy`; si no, peer TCP) y la deja en
/// el header interno. NUNCA falla ni corta: es una anotación.
pub async fn resolver_ip_de_cliente(
    State(estado): State<EstadoServidor>,
    mut request: Request,
    next: Next,
) -> Response {
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(addr)| addr.ip());

    let ip = ip_del_cliente(request.headers(), estado.config.trust_proxy, peer);

    match ip {
        Some(ip) => {
            if let Ok(valor) = HeaderValue::from_str(&ip) {
                request.headers_mut().insert(HEADER_IP, valor);
            }
        }
        None => {
            request.headers_mut().remove(HEADER_IP);
        }
    }

    next.run(request).await
}

/// La lógica en sí (pura, testeable): headers SOLO si se confía; si no, el peer.
pub fn ip_del_cliente(
    headers: &HeaderMap,
    confiar_en_headers: bool,
    peer: Option<IpAddr>,
) -> Option<String> {
    if confiar_en_headers {
        if let Some(ip) = de_los_headers(headers) {
            return Some(ip);
        }
    }
    peer.map(|ip| ip.to_string())
}

fn de_los_headers(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-real-ip")
        .or_else(|| headers.get("x-forwarded-for"))
        .and_then(|valor| valor.to_str().ok())
        .map(|valor| valor.split(',').next().unwrap_or(valor).trim().to_owned())
        .filter(|ip| !ip.is_empty())
}

/// La IP resuelta del request actual (None si el middleware no corrió o no había IP).
pub fn ip_de(headers: &HeaderMap) -> Option<String> {
    headers
        .get(&HEADER_IP)
        .and_then(|valor| valor.to_str().ok())
        .map(str::to_owned)
        .filter(|ip| !ip.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers_con(par: (&'static str, &'static str)) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(par.0, HeaderValue::from_static(par.1));
        headers
    }

    #[test]
    fn sin_confianza_los_headers_no_cuentan() {
        let headers = headers_con(("x-forwarded-for", "198.51.100.9"));
        let peer = Some(IpAddr::from([192, 0, 2, 50]));

        // El header dice 198.51.100.9, pero sin trust_proxy manda el peer real.
        assert_eq!(
            ip_del_cliente(&headers, false, peer).as_deref(),
            Some("192.0.2.50")
        );
    }

    #[test]
    fn sin_confianza_y_sin_peer_queda_none() {
        let headers = headers_con(("x-real-ip", "198.51.100.9"));
        assert_eq!(ip_del_cliente(&headers, false, None), None);
    }

    #[test]
    fn con_confianza_manda_el_header() {
        let headers = headers_con(("x-forwarded-for", "203.0.113.7, 198.51.100.1"));
        let peer = Some(IpAddr::from([127, 0, 0, 1]));

        // El primero de la lista (el cliente original), como documenta el borde.
        assert_eq!(
            ip_del_cliente(&headers, true, peer).as_deref(),
            Some("203.0.113.7")
        );
    }

    #[test]
    fn con_confianza_sin_header_cae_al_peer() {
        let headers = HeaderMap::new();
        let peer = Some(IpAddr::from([192, 0, 2, 50]));
        assert_eq!(
            ip_del_cliente(&headers, true, peer).as_deref(),
            Some("192.0.2.50")
        );
    }

    #[test]
    fn el_header_interno_lo_lee_ip_de() {
        let mut headers = HeaderMap::new();
        headers.insert(HEADER_IP, HeaderValue::from_static("192.0.2.77"));
        assert_eq!(ip_de(&headers).as_deref(), Some("192.0.2.77"));

        // Vacío o ausente: None (nunca un string vacío en la clave del freno).
        headers.insert(HEADER_IP, HeaderValue::from_static(""));
        assert_eq!(ip_de(&headers), None);
        assert_eq!(ip_de(&HeaderMap::new()), None);
    }
}
