use const_format::concatcp;

pub const API_ELEM: &str = "api";
pub const CLIENT_ELEM: &str = "client";

pub const PATH_CSS_DIR: &str = ".css";
pub const PATH_CSS_MAIN_FILE: &str = "main.css";
pub const PATH_CSS_MAIN_PATH: &str = concatcp!(CLIENT_ELEM, "/", PATH_CSS_DIR, "/", PATH_CSS_MAIN_FILE);

pub const GENERIC_404_PAGE: &str = "
<!DOCTYPE html>
<html>
    <head>
        <title>Beelay 404 Not Found</title>
    </head>
    <body>
        <h1>Beelay 404 Not Found</h1>
        <p>Requested resource not found. Please consult Beelay documentation.</p>
    </body>
</html>
";

