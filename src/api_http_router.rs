/*!
 * Routes requests to handlers
 */

use crate::api_error::ApiHttpError;
use crate::api_handler::RouteHandler;

use http::Method;
use http::StatusCode;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use url::Url;

/**
 * `HttpRouter` is a simple data structure for routing incoming HTTP requests to
 * specific handler functions based on the request method and URI path.
 *
 * ## Examples
 *
 * ```
 * use http::Method;
 * use http::StatusCode;
 * use hyper::Body;
 * use hyper::Response;
 * use oxide_api_prototype::api_error::ApiHttpError;
 * use oxide_api_prototype::api_handler::RequestContext;
 * use oxide_api_prototype::api_handler::api_handler_create;
 * use oxide_api_prototype::api_handler::RouteHandler;
 * use oxide_api_prototype::api_http_router::HttpRouter;
 * use oxide_api_prototype::api_http_router::LookupResult;
 * use std::sync::Arc;
 *
 * /// Example HTTP request handler function
 * async fn demo_handler(_: Arc<RequestContext>)
 *     -> Result<Response<Body>, ApiHttpError>
 * {
 *      Ok(Response::builder()
 *          .status(StatusCode::NO_CONTENT)
 *          .body(Body::empty())?)
 * }
 *
 * fn demo()
 *     -> Result<(), ApiHttpError>
 * {
 *      // Create a router and register a few routes.
 *      let mut router = HttpRouter::new();
 *      router.insert(Method::GET, "/projects",
 *          api_handler_create(demo_handler));
 *      router.insert(Method::GET, "/projects/{project_id}",
 *          api_handler_create(demo_handler));
 *
 *      // Basic lookup for a literal path.
 *      let lookup: LookupResult = router.lookup_route(
 *          &Method::GET,
 *          "/projects"
 *      )?;
 *      let handler: &Box<dyn RouteHandler> = lookup.handler;
 *      assert!(lookup.variables.is_empty());
 *      // handler.handle_request(...)
 *
 *      // Basic lookup with path variables
 *      let lookup: LookupResult = router.lookup_route(
 *          &Method::GET,
 *          "/projects/proj123"
 *      )?;
 *      assert_eq!(
 *          *lookup.variables.get(&"project_id".to_string()).unwrap(),
 *          "proj123".to_string()
 *      );
 *      let handler: &Box<dyn RouteHandler> = lookup.handler;
 *      // handler.handle_request(...)
 *
 *      // If a route is not found, we get back a 404.
 *      let error = router.lookup_route(&Method::GET, "/foo").unwrap_err();
 *      assert_eq!(error.status_code, StatusCode::NOT_FOUND);
 *
 *      // If a route is found, but there's no handler for this method,
 *      // we get back a 405.
 *      let error = router.lookup_route(&Method::PUT, "/projects").unwrap_err();
 *      assert_eq!(error.status_code, StatusCode::METHOD_NOT_ALLOWED);
 *      Ok(())
 * }
 * ```
 *
 * ## Usage details
 *
 * Routes are registered and looked up according to a path, like `"/foo/bar"`.
 * Paths are split into segments separated by one or more '/' characters.  When
 * registering a route, a path segment may be either a literal string or a
 * variable.  Variables are specified by wrapping the segment in braces.
 *
 * For example, a handler registered for `"/foo/bar"` will match only
 * `"/foo/bar"` (after normalization, that is -- it will also match
 * `"/foo///bar"`).  A handler registered for `"/foo/{bar}"` uses a
 * variable for the second segment, so it will match `"/foo/123"` (with `"bar"`
 * assigned to `"123"`) as well as `"/foo/bar456"` (with `"bar"` mapped to
 * `"bar456"`).  Only one segment is matched per variable, so `"/foo/{bar}"`
 * will not match `"/foo/123/456"`.
 *
 * The implementation here is essentially a trie where edges represent segments
 * of the URI path.  ("Segments" here are chunks of the path separated by one or
 * more "/" characters.)  To register or look up the path `"/foo/bar/baz"`, we
 * would start at the root and traverse edges for the literal strings `"foo"`,
 * `"bar"`, and `"baz"`, arriving at a particular node.  Each node has a set of
 * handlers, each associated with one HTTP method.
 *
 * We make (and, in some cases, enforce) a number of simplifying assumptions.
 * These could be relaxed, but it's not clear that's useful, and enforcing them
 * makes it easier to catch some types of bugs:
 *
 * * A particular resource (node) may have child resources (edges) with either
 *   literal path segments or variable path segments, but not both.  For
 *   example, you can't register both `"/projects/{id}"` and
 *   `"/projects/default"`.
 *
 * * If a given resource has an edge with a variable name, all routes through
 *   this node must use the same name for that variable.  That is, you can't
 *   define routes for `"/projects/{id}"` and `"/projects/{project_id}/info"`.
 *
 * * A given path cannot use the same variable name twice.  For example, you
 *   can't register path `"/projects/{id}/instances/{id}"`.
 *
 * * A given resource may have at most one handler for a given HTTP method.
 *
 * * The expectation is that during server initialization,
 *   `HttpRouter::insert()` will be invoked to register a number of route
 *   handlers.  After that initialization period, the router will be
 *   read-only.  This behavior isn't enforced by `HttpRouter`.
 */
#[derive(Debug)]
pub struct HttpRouter {
    /** root of the trie */
    root: Box<HttpRouterNode>
}

/**
 * Each node in the tree represents a group of HTTP resources having the same
 * handler functions.  As described above, these may correspond to exactly one
 * canonical path (e.g., `"/foo/bar"`) or a set of paths that differ by some
 * number of variable assignments (e.g., `"/projects/123/instances"` and
 * `"/projects/456/instances"`).
 *
 * Edges of the tree come in one of type types: edges for literal strings and
 * edges for variable strings.  A given node has either literal string edges or
 * variable edges, but not both.  However, we don't necessarily know what type
 * of outgoing edges a node will have when we create it.
 */
#[derive(Debug)]
struct HttpRouterNode {
    /** Handlers for each of the HTTP methods defined for this node. */
    method_handlers: BTreeMap<String, Box<dyn RouteHandler>>,
    /** Outgoing edges for different literal paths. */
    edges_literals: BTreeMap<String, Box<HttpRouterNode>>,
    /** Outgoing edges for variable-named paths. */
    edge_varname: Option<HttpRouterEdgeVariable>
}

/**
 * Represents an outgoing edge having a variable name.  (See the `HttpRouter`
 * comments for details.)  This is just used to group the variable name and the
 * Node pointer.  There's no corresponding struct for literal-named edges
 * because they don't have any data aside from the Node pointer.
 */
#[derive(Debug)]
struct HttpRouterEdgeVariable(String, Box<HttpRouterNode>);

/**
 * `PathSegment` represents a segment in a URI path when the router is being
 * configured.  Each segment may be either a literal string or a variable (the
 * latter indicated by being wrapped in braces.
 */
#[derive(Debug)]
enum PathSegment {
    /** a path segment for a literal string */
    Literal(String),
    /** a path segment for a variable */
    Varname(String)
}

impl PathSegment {
    /**
     * Given a `&String` representing a path segment from a Uri, return a
     * PathSegment.  This is used to parse a sequence of path segments to the
     * corresponding `PathSegment`, which basically means determining whether
     * it's a variable or a literal.
     */
    fn from(segment: &String)
        -> PathSegment
    {
        /*
         * TODO-cleanup use of percent-encoding here
         * TODO-correctness figure out if we _should_ be using percent-encoding
         * here or not -- i.e., is the matching actually correct?
         */
        if !segment.starts_with("%7B")
            || !segment.ends_with("%7D")
            || segment.chars().count() < 7 {
            PathSegment::Literal(segment.to_string())
        } else {
            let segment_chars: Vec<char> = segment.chars().collect();
            let newlast = segment_chars.len() - 3;
            let varname_chars = &segment_chars[3..newlast];
            PathSegment::Varname(varname_chars.iter().collect())
        }
    }
}

/**
 * `LookupResult` represents the result of invoking
 * `HttpRouter::lookup_route()`.  A successful route lookup includes both the
 * handler and a mapping of variables in the configured path to the
 * corresponding values in the actual path.
 */
#[derive(Debug)]
pub struct LookupResult<'a> {
    pub handler: &'a Box<dyn RouteHandler>,
    pub variables: BTreeMap<String, String>,
}

impl HttpRouter {
    /**
     * Returns a new `HttpRouter` with no routes configured.
     */
    pub fn new()
        -> Self
    {
        HttpRouter {
            root: Box::new(HttpRouterNode {
                method_handlers: BTreeMap::new(),
                edges_literals: BTreeMap::new(),
                edge_varname: None
            })
        }
    }

    /**
     * Helper function for taking a Uri path and producing a `Vec<String>` of
     * URL-encoded strings, each representing one segment of the path.
     */
    fn path_to_segments(path: &str)
        -> Vec<String>
    {
        /* TODO-cleanup is this really the right way?  Feels like a hack. */
        let base = Url::parse("http://127.0.0.1/").unwrap();
        let url = match base.join(path) {
            Ok(parsed) => parsed,
            Err(e) => {
                panic!("attempted to create route for invalid URL: {}: \"{}\"",
                    path, e);
            }
        };

        /*
         * TODO-correctness is it possible for bad input to cause this to fail?
         * If so, we should provide a better error message.
         */
        url.path_segments().unwrap().map(String::from).collect()
    }

    /**
     * Configure a route for HTTP requests based on the HTTP `method` and
     * URL `path`.  See the `HttpRouter` docs for information about how `path`
     * is processed.  Requests matching `path` will be resolved to `handler`.
     */
    pub fn insert(&mut self, method: Method, path: &str,
        handler: Box<dyn RouteHandler>)
    {
        let all_segments = HttpRouter::path_to_segments(path);
        let mut varnames: BTreeSet<String> = BTreeSet::new();

        let mut node: &mut Box<HttpRouterNode> = &mut self.root;
        for raw_segment in all_segments {
            let segment = PathSegment::from(&raw_segment);

            node = match segment {
                PathSegment::Literal(lit) => {
                    /*
                     * We do not allow both literal and variable edges from the
                     * same node.  This could be supported (with some caveats
                     * about how matching would work), but it seems more likely
                     * to be a mistake.
                     */
                    if let Some(HttpRouterEdgeVariable(varname, _)) =
                        &node.edge_varname {
                        panic!("URL path \"{}\": attempted to register route \
                            for literal path segment \"{}\" when a route \
                            exists for variable path segment (variable name: \
                            \"{}\")", path, lit, varname);
                    }

                    if !node.edges_literals.contains_key(&lit) {
                        let newnode = Box::new(HttpRouterNode {
                            method_handlers: BTreeMap::new(),
                            edges_literals: BTreeMap::new(),
                            edge_varname: None
                        });

                        node.edges_literals.insert(lit.clone(), newnode);
                    }

                    node.edges_literals.get_mut(&lit).unwrap()
                },

                PathSegment::Varname(new_varname) => {
                    /*
                     * See the analogous check above about combining literal and
                     * variable path segments from the same resource.
                     */
                    if ! node.edges_literals.is_empty() {
                        panic!("URL path \"{}\": attempted to register route \
                            for variable path segment (variable name: \"{}\") \
                            when a route already exists for a literal path \
                            segment", path, new_varname);
                    }

                    /*
                     * Do not allow the same variable name to be used more than
                     * once in the path.  Again, this could be supported (with
                     * some caveats), but it seems more likely to be a mistake.
                     */
                    if varnames.contains(&new_varname) {
                        panic!("URL path \"{}\": variable name \"{}\" is used \
                            more than once", path, new_varname);
                    }
                    varnames.insert(new_varname.clone());

                    if node.edge_varname.is_none() {
                        let newnode = Box::new(HttpRouterNode {
                            method_handlers: BTreeMap::new(),
                            edges_literals: BTreeMap::new(),
                            edge_varname: None
                        });

                        node.edge_varname = Some(HttpRouterEdgeVariable(
                            new_varname.clone(), newnode));
                    } else if *new_varname !=
                            *node.edge_varname.as_ref().unwrap().0 {
                        /*
                         * Don't allow people to use different names for the
                         * same part of the path.  Again, this could be
                         * supported, but it seems likely to be confusing and
                         * probably a mistake.
                         */
                        panic!("URL path \"{}\": attempted to use variable \
                            name \"{}\", but a different name (\"{}\") has \
                            already been used for this", path, new_varname,
                            node.edge_varname.as_ref().unwrap().0);
                    }

                    &mut node.edge_varname.as_mut().unwrap().1
                }
            };
        }

        let methodname = method.as_str().to_uppercase();
        if let Some(_) = node.method_handlers.get(&methodname) {
            panic!("URL path \"{}\": attempted to create duplicate route for \
                method \"{}\"", path, method);
        }

        node.method_handlers.insert(methodname, handler);
    }

    /**
     * Look up the route handler for an HTTP request having method `method` and
     * URL path `path`.  A successful lookup produces a `LookupResult`, which
     * includes both the handler that can process this request and a map of
     * variables assigned based on the request path as part of the lookup.  On
     * failure, this returns an `ApiHttpError` appropriate for the failure mode.
     *
     * TODO-cleanup
     * consider defining a separate struct type for url-encoded vs. not?
     */
    pub fn lookup_route<'a, 'b>(&'a self, method: &'b Method, path: &'b str)
        -> Result<LookupResult<'a>, ApiHttpError>
    {
        let all_segments = HttpRouter::path_to_segments(path);
        let mut node: &Box<HttpRouterNode> = &self.root;
        let mut variables: BTreeMap<String, String> = BTreeMap::new();

        for segment in all_segments {
            let segment_string = segment.to_string();
            if let Some(n) = node.edges_literals.get(&segment_string) {
                node = n;
            } else if let Some(edge) = &node.edge_varname {
                variables.insert(edge.0.clone(), segment_string);
                node = &edge.1
            } else {
                return Err(ApiHttpError::for_status(StatusCode::NOT_FOUND))
            }
        }

        let methodname = method.as_str().to_uppercase();
        if let Some(handler) = node.method_handlers.get(&methodname) {
            Ok(LookupResult {
                handler: handler,
                variables: variables
            })
        } else {
            Err(ApiHttpError::for_status(StatusCode::METHOD_NOT_ALLOWED))
        }
    }
}

#[cfg(test)]
mod test {
    use crate::api_error::ApiHttpError;
    use crate::api_handler::api_handler_create;
    use crate::api_handler::RouteHandler;
    use crate::api_handler::RequestContext;
    use hyper::Response;
    use hyper::Body;
    use std::sync::Arc;
    use http::Method;
    use super::HttpRouter;

    async fn test_handler(_: Arc<RequestContext>)
        -> Result<Response<Body>, ApiHttpError>
    {
        panic!("test handler is not supposed to run");
    }

    fn new_handler()
        -> Box<dyn RouteHandler>
    {
        api_handler_create(test_handler)
    }

    #[test]
    #[should_panic(expected = "URL path \"/boo\": attempted to create \
        duplicate route for method \"GET\"")]
    fn test_duplicate_route()
    {
        let mut router = HttpRouter::new();
        router.insert(Method::GET, "/boo", new_handler());
        router.insert(Method::GET, "/boo", new_handler());
    }

    #[test]
    #[should_panic(expected = "URL path \"/projects/{id}/insts/{id}\": \
        variable name \"id\" is used more than once")]
    fn test_duplicate_varname()
    {
        let mut router = HttpRouter::new();
        router.insert(Method::GET, "/projects/{id}/insts/{id}", new_handler());
    }

    #[test]
    #[should_panic(expected = "URL path \"/projects/{id}\": attempted to use \
        variable name \"id\", but a different name (\"project_id\") has \
        already been used for this")]
    fn test_inconsistent_varname()
    {
        let mut router = HttpRouter::new();
        router.insert(Method::GET, "/projects/{project_id}", new_handler());
        router.insert(Method::GET, "/projects/{id}", new_handler());
    }

    #[test]
    #[should_panic(expected = "URL path \"/projects/{id}\": attempted to \
        register route for variable path segment (variable name: \"id\") when \
        a route already exists for a literal path segment")]
    fn test_variable_after_literal()
    {
        let mut router = HttpRouter::new();
        router.insert(Method::GET, "/projects/default", new_handler());
        router.insert(Method::GET, "/projects/{id}", new_handler());
    }

    #[test]
    #[should_panic(expected = "URL path \"/projects/default\": attempted to \
        register route for literal path segment \"default\" when a route \
        exists for variable path segment (variable name: \"id\")")]
    fn test_literal_after_variable()
    {
        let mut router = HttpRouter::new();
        router.insert(Method::GET, "/projects/{id}", new_handler());
        router.insert(Method::GET, "/projects/default", new_handler());
    }

    #[test]
    fn test_router()
    {
        let mut router = HttpRouter::new();

        eprintln!("router: {:?}", router);
        router.insert(Method::GET, "/foo/{bar}/baz", new_handler());
        router.insert(Method::GET, "/boo", new_handler());
        eprintln!("router: {:?}", router);
    }
}
