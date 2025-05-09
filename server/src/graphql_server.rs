use crate::{auth_service::check_if_token_is_valid, tcp_server::AppState};
use actix_web::FromRequest;
use actix_web::HttpMessage;
use actix_web::{Error, HttpRequest, HttpResponse, error::JsonPayloadError, web};
use actix_web_httpauth::extractors::bearer::BearerAuth;
use juniper::{
    ScalarValue,
    http::{
        GraphQLBatchRequest, GraphQLRequest, graphiql::graphiql_source,
        playground::playground_source,
    },
};
use lldap_domain_handlers::handler::BackendHandler;
use lldap_graphql_server::api::Context;
use lldap_graphql_server::api::schema;

async fn graphiql_route() -> Result<HttpResponse, Error> {
    let html = graphiql_source("/api/graphql", None);
    Ok(HttpResponse::Ok()
        .content_type("text/html; charset=utf-8")
        .body(html))
}
async fn playground_route() -> Result<HttpResponse, Error> {
    let html = playground_source("/api/graphql", None);
    Ok(HttpResponse::Ok()
        .content_type("text/html; charset=utf-8")
        .body(html))
}

#[derive(serde::Deserialize, Clone, PartialEq, Debug)]
struct GetGraphQLRequest {
    query: String,
    #[serde(rename = "operationName")]
    operation_name: Option<String>,
    variables: Option<String>,
}

impl<S> From<GetGraphQLRequest> for GraphQLRequest<S>
where
    S: ScalarValue,
{
    fn from(get_req: GetGraphQLRequest) -> Self {
        let GetGraphQLRequest {
            query,
            operation_name,
            variables,
        } = get_req;
        let variables = variables.map(|s| serde_json::from_str(&s).unwrap());
        Self::new(query, operation_name, variables)
    }
}

/// Actix GraphQL Handler for GET requests
pub async fn get_graphql_handler<Query, Mutation, Subscription, CtxT, S>(
    schema: &juniper::RootNode<'static, Query, Mutation, Subscription, S>,
    context: &CtxT,
    req: HttpRequest,
) -> Result<HttpResponse, Error>
where
    Query: juniper::GraphQLTypeAsync<S, Context = CtxT>,
    Query::TypeInfo: Sync,
    Mutation: juniper::GraphQLTypeAsync<S, Context = CtxT>,
    Mutation::TypeInfo: Sync,
    Subscription: juniper::GraphQLSubscriptionType<S, Context = CtxT>,
    Subscription::TypeInfo: Sync,
    CtxT: Sync,
    S: ScalarValue + Send + Sync,
{
    let get_req = web::Query::<GetGraphQLRequest>::from_query(req.query_string())?;
    let req = GraphQLRequest::from(get_req.into_inner());
    let gql_response = req.execute(schema, context).await;
    let body_response = serde_json::to_string(&gql_response)?;
    let mut response = match gql_response.is_ok() {
        true => HttpResponse::Ok(),
        false => HttpResponse::BadRequest(),
    };
    Ok(response
        .content_type("application/json")
        .body(body_response))
}

/// Actix GraphQL Handler for POST requests
pub async fn post_graphql_handler<Query, Mutation, Subscription, CtxT, S>(
    schema: &juniper::RootNode<'static, Query, Mutation, Subscription, S>,
    context: &CtxT,
    req: HttpRequest,
    mut payload: actix_http::Payload,
) -> Result<HttpResponse, Error>
where
    Query: juniper::GraphQLTypeAsync<S, Context = CtxT>,
    Query::TypeInfo: Sync,
    Mutation: juniper::GraphQLTypeAsync<S, Context = CtxT>,
    Mutation::TypeInfo: Sync,
    Subscription: juniper::GraphQLSubscriptionType<S, Context = CtxT>,
    Subscription::TypeInfo: Sync,
    CtxT: Sync,
    S: ScalarValue + Send + Sync,
{
    let req = match req.content_type() {
        "application/json" => {
            let body = String::from_request(&req, &mut payload).await?;
            serde_json::from_str::<GraphQLBatchRequest<S>>(&body)
                .map_err(JsonPayloadError::Deserialize)
        }
        "application/graphql" => {
            let body = String::from_request(&req, &mut payload).await?;
            Ok(GraphQLBatchRequest::Single(GraphQLRequest::new(
                body, None, None,
            )))
        }
        _ => Err(JsonPayloadError::ContentType),
    }?;
    let gql_batch_response = req.execute(schema, context).await;
    let gql_response = serde_json::to_string(&gql_batch_response)?;
    let mut response = match gql_batch_response.is_ok() {
        true => HttpResponse::Ok(),
        false => HttpResponse::BadRequest(),
    };
    Ok(response.content_type("application/json").body(gql_response))
}

async fn graphql_route<Handler: BackendHandler + Clone>(
    req: actix_web::HttpRequest,
    payload: actix_web::web::Payload,
    data: web::Data<AppState<Handler>>,
) -> Result<HttpResponse, Error> {
    let mut inner_payload = payload.into_inner();
    let bearer = BearerAuth::from_request(&req, &mut inner_payload).await?;
    let validation_result = check_if_token_is_valid(&data, bearer.token())?;
    let context = Context::<Handler> {
        handler: data.backend_handler.clone(),
        validation_result,
    };
    let schema = &schema();
    let context = &context;
    match *req.method() {
        actix_http::Method::POST => post_graphql_handler(schema, context, req, inner_payload).await,
        actix_http::Method::GET => get_graphql_handler(schema, context, req).await,
        _ => Err(actix_web::error::UrlGenerationError::ResourceNotFound.into()),
    }
}

pub fn configure_endpoint<Backend>(cfg: &mut web::ServiceConfig)
where
    Backend: BackendHandler + Clone + 'static,
{
    let json_config = web::JsonConfig::default()
        .limit(4096)
        .error_handler(|err, _req| {
            // create custom error response
            log::error!("API error: {}", err);
            let msg = err.to_string();
            actix_web::error::InternalError::from_response(
                err,
                HttpResponse::BadRequest().body(msg),
            )
            .into()
        });
    cfg.app_data(json_config);
    cfg.app_data(web::PayloadConfig::new(1 << 24)); // Max payload size: 16MB, allows for a 12MB image.
    cfg.service(
        web::resource("/graphql")
            .route(web::post().to(graphql_route::<Backend>))
            .route(web::get().to(graphql_route::<Backend>)),
    );
    cfg.service(web::resource("/graphql/playground").route(web::get().to(playground_route)));
    cfg.service(web::resource("/graphql/graphiql").route(web::get().to(graphiql_route)));
}
