//! API Gateway resource kinds. REST APIs (v1) and HTTP/WebSocket APIs (v2) are
//! separate AWS APIs; they're shown as one list, and their routes as one view.

mod apis;
mod routes;

pub use apis::Apis;
pub use routes::Routes;

/// Registers every API Gateway resource kind.
pub fn register(registry: &mut fast_aws_core::Registry) {
    registry.register(Apis);
    registry.register(Routes);
}

/// Which API Gateway API a row came from. Encoded in row and parent IDs as
/// `rest:<id>` / `v2:<id>`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Flavor {
    Rest,
    V2,
}

fn tagged_id(flavor: Flavor, id: &str) -> String {
    match flavor {
        Flavor::Rest => format!("rest:{id}"),
        Flavor::V2 => format!("v2:{id}"),
    }
}

fn split_id(tagged: &str) -> (Flavor, &str) {
    match tagged.split_once(':') {
        Some(("rest", id)) => (Flavor::Rest, id),
        Some(("v2", id)) => (Flavor::V2, id),
        _ => (Flavor::Rest, tagged),
    }
}

/// `arn:...:lambda:path/2015-03-31/functions/<fn-arn>/invocations` → the
/// function's name; other integration URIs are returned as they are.
fn integration_target(uri: &str) -> String {
    if let Some(start) = uri.find("functions/") {
        let function_arn = uri[start + "functions/".len()..].trim_end_matches("/invocations");
        let name = function_arn.split(':').nth(6).unwrap_or(function_arn);
        return format!("λ {name}");
    }
    if uri.starts_with("arn:aws:lambda:") {
        let name = uri.split(':').nth(6).unwrap_or(uri);
        return format!("λ {name}");
    }
    uri.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_carry_their_flavor() {
        assert_eq!(
            split_id(&tagged_id(Flavor::Rest, "abc")),
            (Flavor::Rest, "abc")
        );
        assert_eq!(split_id(&tagged_id(Flavor::V2, "xyz")), (Flavor::V2, "xyz"));
    }

    #[test]
    fn lambda_integrations_show_the_function_name() {
        let v1 = "arn:aws:apigateway:eu-west-1:lambda:path/2015-03-31/functions/arn:aws:lambda:eu-west-1:123:function:orders/invocations";
        assert_eq!(integration_target(v1), "λ orders");
        assert_eq!(
            integration_target("arn:aws:lambda:eu-west-1:123:function:orders"),
            "λ orders"
        );
        assert_eq!(
            integration_target("https://example.com/{proxy}"),
            "https://example.com/{proxy}"
        );
    }
}
