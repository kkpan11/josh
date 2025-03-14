use std::{error::Error, fmt, string::FromUtf8Error, sync::Arc};

use hyper::{
    Body, Method, Request, Response, StatusCode,
    header::{self, HeaderValue},
};
use juniper::{
    GraphQLSubscriptionType, GraphQLType, GraphQLTypeAsync, InputValue, RootNode, ScalarValue,
    http::{GraphQLBatchRequest, GraphQLRequest as JuniperGraphQLRequest, GraphQLRequest},
};
use serde_json::error::Error as SerdeError;
use url::form_urlencoded;

pub async fn graphql_sync<CtxT, QueryT, MutationT, SubscriptionT, S>(
    root_node: Arc<RootNode<'static, QueryT, MutationT, SubscriptionT, S>>,
    context: Arc<CtxT>,
    req: Request<Body>,
) -> Result<Response<Body>, hyper::Error>
where
    QueryT: GraphQLType<S, Context = CtxT>,
    QueryT::TypeInfo: Sync,
    MutationT: GraphQLType<S, Context = CtxT>,
    MutationT::TypeInfo: Sync,
    SubscriptionT: GraphQLType<S, Context = CtxT>,
    SubscriptionT::TypeInfo: Sync,
    CtxT: Sync,
    S: ScalarValue + Send + Sync,
{
    Ok(match parse_req(req).await {
        Ok(req) => execute_request_sync(root_node, context, req).await,
        Err(resp) => resp,
    })
}

pub async fn graphql<CtxT, QueryT, MutationT, SubscriptionT, S>(
    root_node: Arc<RootNode<'static, QueryT, MutationT, SubscriptionT, S>>,
    context: Arc<CtxT>,
    req: Request<Body>,
) -> Result<Response<Body>, hyper::Error>
where
    QueryT: GraphQLTypeAsync<S, Context = CtxT>,
    QueryT::TypeInfo: Sync,
    MutationT: GraphQLTypeAsync<S, Context = CtxT>,
    MutationT::TypeInfo: Sync,
    SubscriptionT: GraphQLSubscriptionType<S, Context = CtxT>,
    SubscriptionT::TypeInfo: Sync,
    CtxT: Sync,
    S: ScalarValue + Send + Sync,
{
    Ok(match parse_req(req).await {
        Ok(req) => execute_request(root_node, context, req).await,
        Err(resp) => resp,
    })
}

pub async fn parse_req<S: ScalarValue>(
    req: Request<Body>,
) -> Result<GraphQLBatchRequest<S>, Response<Body>> {
    match *req.method() {
        Method::GET => parse_get_req(req),
        Method::POST => {
            let content_type = req
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|x| HeaderValue::to_str(x).ok())
                .and_then(|x| x.split(';').next());
            match content_type {
                Some("application/json") => parse_post_json_req(req.into_body()).await,
                Some("application/graphql") => parse_post_graphql_req(req.into_body()).await,
                _ => return Err(new_response(StatusCode::BAD_REQUEST)),
            }
        }
        _ => return Err(new_response(StatusCode::METHOD_NOT_ALLOWED)),
    }
    .map_err(render_error)
}

fn parse_get_req<S: ScalarValue>(
    req: Request<Body>,
) -> Result<GraphQLBatchRequest<S>, GraphQLRequestError> {
    req.uri()
        .query()
        .map(|q| gql_request_from_get(q).map(GraphQLBatchRequest::Single))
        .unwrap_or_else(|| {
            Err(GraphQLRequestError::Invalid(
                "'query' parameter is missing".to_string(),
            ))
        })
}

async fn parse_post_json_req<S: ScalarValue>(
    body: Body,
) -> Result<GraphQLBatchRequest<S>, GraphQLRequestError> {
    let chunk = hyper::body::to_bytes(body)
        .await
        .map_err(GraphQLRequestError::BodyHyper)?;

    let input = String::from_utf8(chunk.iter().cloned().collect())
        .map_err(GraphQLRequestError::BodyUtf8)?;

    serde_json::from_str::<GraphQLBatchRequest<S>>(&input)
        .map_err(GraphQLRequestError::BodyJSONError)
}

async fn parse_post_graphql_req<S: ScalarValue>(
    body: Body,
) -> Result<GraphQLBatchRequest<S>, GraphQLRequestError> {
    let chunk = hyper::body::to_bytes(body)
        .await
        .map_err(GraphQLRequestError::BodyHyper)?;

    let query = String::from_utf8(chunk.iter().cloned().collect())
        .map_err(GraphQLRequestError::BodyUtf8)?;

    Ok(GraphQLBatchRequest::Single(GraphQLRequest::new(
        query, None, None,
    )))
}

pub fn graphiql(
    graphql_endpoint: &str,
    subscriptions_endpoint: Option<&str>,
) -> Result<Response<Body>, hyper::Error> {
    let mut resp = new_html_response(StatusCode::OK);
    // XXX: is the call to graphiql_source blocking?
    *resp.body_mut() = Body::from(juniper::http::graphiql::graphiql_source(
        graphql_endpoint,
        subscriptions_endpoint,
    ));
    Ok(resp)
}

pub async fn playground(
    graphql_endpoint: &str,
    subscriptions_endpoint: Option<&str>,
) -> Result<Response<Body>, hyper::Error> {
    let mut resp = new_html_response(StatusCode::OK);
    *resp.body_mut() = Body::from(juniper::http::playground::playground_source(
        graphql_endpoint,
        subscriptions_endpoint,
    ));
    Ok(resp)
}

fn render_error(err: GraphQLRequestError) -> Response<Body> {
    let message = format!("{}", err);
    let mut resp = new_response(StatusCode::BAD_REQUEST);
    *resp.body_mut() = Body::from(message);
    resp
}

async fn execute_request_sync<CtxT, QueryT, MutationT, SubscriptionT, S>(
    root_node: Arc<RootNode<'static, QueryT, MutationT, SubscriptionT, S>>,
    context: Arc<CtxT>,
    request: GraphQLBatchRequest<S>,
) -> Response<Body>
where
    QueryT: GraphQLType<S, Context = CtxT>,
    QueryT::TypeInfo: Sync,
    MutationT: GraphQLType<S, Context = CtxT>,
    MutationT::TypeInfo: Sync,
    SubscriptionT: GraphQLType<S, Context = CtxT>,
    SubscriptionT::TypeInfo: Sync,
    CtxT: Sync,
    S: ScalarValue + Send + Sync,
{
    let res = request.execute_sync(&*root_node, &context);
    let body = Body::from(serde_json::to_string_pretty(&res).unwrap());
    let code = if res.is_ok() {
        StatusCode::OK
    } else {
        StatusCode::BAD_REQUEST
    };
    let mut resp = new_response(code);
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    *resp.body_mut() = body;
    resp
}

pub async fn execute_request<CtxT, QueryT, MutationT, SubscriptionT, S>(
    root_node: Arc<RootNode<'static, QueryT, MutationT, SubscriptionT, S>>,
    context: Arc<CtxT>,
    request: GraphQLBatchRequest<S>,
) -> Response<Body>
where
    QueryT: GraphQLTypeAsync<S, Context = CtxT>,
    QueryT::TypeInfo: Sync,
    MutationT: GraphQLTypeAsync<S, Context = CtxT>,
    MutationT::TypeInfo: Sync,
    SubscriptionT: GraphQLSubscriptionType<S, Context = CtxT>,
    SubscriptionT::TypeInfo: Sync,
    CtxT: Sync,
    S: ScalarValue + Send + Sync,
{
    let res = request.execute(&*root_node, &context).await;
    let body = Body::from(serde_json::to_string_pretty(&res).unwrap());
    let code = if res.is_ok() {
        StatusCode::OK
    } else {
        StatusCode::BAD_REQUEST
    };
    let mut resp = new_response(code);
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    *resp.body_mut() = body;
    resp
}

fn gql_request_from_get<S>(input: &str) -> Result<JuniperGraphQLRequest<S>, GraphQLRequestError>
where
    S: ScalarValue,
{
    let mut query = None;
    let operation_name = None;
    let mut variables = None;
    for (key, value) in form_urlencoded::parse(input.as_bytes()).into_owned() {
        match key.as_ref() {
            "query" => {
                if query.is_some() {
                    return Err(invalid_err("query"));
                }
                query = Some(value)
            }
            "operationName" => {
                if operation_name.is_some() {
                    return Err(invalid_err("operationName"));
                }
            }
            "variables" => {
                if variables.is_some() {
                    return Err(invalid_err("variables"));
                }
                match serde_json::from_str::<InputValue<S>>(&value)
                    .map_err(GraphQLRequestError::Variables)
                {
                    Ok(parsed_variables) => variables = Some(parsed_variables),
                    Err(e) => return Err(e),
                }
            }
            _ => continue,
        }
    }
    match query {
        Some(query) => Ok(JuniperGraphQLRequest::new(query, operation_name, variables)),
        None => Err(GraphQLRequestError::Invalid(
            "'query' parameter is missing".to_string(),
        )),
    }
}

fn invalid_err(parameter_name: &str) -> GraphQLRequestError {
    GraphQLRequestError::Invalid(format!(
        "'{}' parameter is specified multiple times",
        parameter_name
    ))
}

fn new_response(code: StatusCode) -> Response<Body> {
    let mut r = Response::new(Body::empty());
    *r.status_mut() = code;
    r
}

fn new_html_response(code: StatusCode) -> Response<Body> {
    let mut resp = new_response(code);
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    resp
}

#[derive(Debug)]
enum GraphQLRequestError {
    BodyHyper(hyper::Error),
    BodyUtf8(FromUtf8Error),
    BodyJSONError(SerdeError),
    Variables(SerdeError),
    Invalid(String),
}

impl fmt::Display for GraphQLRequestError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            GraphQLRequestError::BodyHyper(ref err) => fmt::Display::fmt(err, f),
            GraphQLRequestError::BodyUtf8(ref err) => fmt::Display::fmt(err, f),
            GraphQLRequestError::BodyJSONError(ref err) => fmt::Display::fmt(err, f),
            GraphQLRequestError::Variables(ref err) => fmt::Display::fmt(err, f),
            GraphQLRequestError::Invalid(ref err) => fmt::Display::fmt(err, f),
        }
    }
}

impl Error for GraphQLRequestError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match *self {
            GraphQLRequestError::BodyHyper(ref err) => Some(err),
            GraphQLRequestError::BodyUtf8(ref err) => Some(err),
            GraphQLRequestError::BodyJSONError(ref err) => Some(err),
            GraphQLRequestError::Variables(ref err) => Some(err),
            GraphQLRequestError::Invalid(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use hyper::{
        Body, Method, Response, Server, StatusCode,
        service::{make_service_fn, service_fn},
    };
    use juniper::{
        EmptyMutation, EmptySubscription, RootNode,
        http::tests as http_tests,
        tests::fixtures::starwars::schema::{Database, Query},
    };
    use reqwest::{self, blocking::Response as ReqwestResponse};
    use std::{net::SocketAddr, sync::Arc, thread, time::Duration};

    struct TestHyperIntegration {
        port: u16,
    }

    impl http_tests::HttpIntegration for TestHyperIntegration {
        fn get(&self, url: &str) -> http_tests::TestResponse {
            let url = format!("http://127.0.0.1:{}/graphql{}", self.port, url);
            make_test_response(
                reqwest::blocking::get(&url).unwrap_or_else(|_| panic!("failed GET {}", url)),
            )
        }

        fn post_json(&self, url: &str, body: &str) -> http_tests::TestResponse {
            let url = format!("http://127.0.0.1:{}/graphql{}", self.port, url);
            let client = reqwest::blocking::Client::new();
            let res = client
                .post(&url)
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(body.to_string())
                .send()
                .unwrap_or_else(|_| panic!("failed POST {}", url));
            make_test_response(res)
        }

        fn post_graphql(&self, url: &str, body: &str) -> http_tests::TestResponse {
            let url = format!("http://127.0.0.1:{}/graphql{}", self.port, url);
            let client = reqwest::blocking::Client::new();
            let res = client
                .post(&url)
                .header(reqwest::header::CONTENT_TYPE, "application/graphql")
                .body(body.to_string())
                .send()
                .unwrap_or_else(|_| panic!("failed POST {}", url));
            make_test_response(res)
        }
    }

    fn make_test_response(response: ReqwestResponse) -> http_tests::TestResponse {
        let status_code = response.status().as_u16() as i32;
        let content_type_header = response.headers().get(reqwest::header::CONTENT_TYPE);
        let content_type = if let Some(ct) = content_type_header {
            ct.to_str().unwrap().to_string()
        } else {
            String::default()
        };
        let body = response.text().unwrap();

        http_tests::TestResponse {
            status_code,
            body: Some(body),
            content_type,
        }
    }

    async fn run_hyper_integration(is_sync: bool) {
        let port = if is_sync { 3002 } else { 3001 };
        let addr: SocketAddr = ([127, 0, 0, 1], port).into();

        let db = Arc::new(Database::new());
        let root_node = Arc::new(RootNode::new(
            Query,
            EmptyMutation::<Database>::new(),
            EmptySubscription::<Database>::new(),
        ));

        let new_service = make_service_fn(move |_| {
            let root_node = root_node.clone();
            let ctx = db.clone();

            async move {
                Ok::<_, hyper::Error>(service_fn(move |req| {
                    let root_node = root_node.clone();
                    let ctx = ctx.clone();
                    let matches = {
                        let path = req.uri().path();
                        match req.method() {
                            &Method::POST | &Method::GET => {
                                path == "/graphql" || path == "/graphql/"
                            }
                            _ => false,
                        }
                    };
                    async move {
                        if matches {
                            if is_sync {
                                super::graphql_sync(root_node, ctx, req).await
                            } else {
                                super::graphql(root_node, ctx, req).await
                            }
                        } else {
                            let mut resp = Response::new(Body::empty());
                            *resp.status_mut() = StatusCode::NOT_FOUND;
                            Ok(resp)
                        }
                    }
                }))
            }
        });

        let (shutdown_fut, shutdown) = futures::future::abortable(async {
            tokio::time::sleep(Duration::from_secs(60)).await;
        });

        let server = Server::bind(&addr)
            .serve(new_service)
            .with_graceful_shutdown(async {
                shutdown_fut.await.unwrap_err();
            });

        tokio::task::spawn_blocking(move || {
            thread::sleep(Duration::from_millis(10)); // wait 10ms for server to bind
            let integration = TestHyperIntegration { port };
            http_tests::run_http_test_suite(&integration);
            shutdown.abort();
        });

        if let Err(e) = server.await {
            eprintln!("server error: {}", e);
        }
    }

    #[tokio::test]
    async fn test_hyper_integration() {
        run_hyper_integration(false).await
    }

    #[tokio::test]
    async fn test_sync_hyper_integration() {
        run_hyper_integration(true).await
    }
}
