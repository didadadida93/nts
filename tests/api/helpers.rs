use nts::configuration::{get_configuration, DatabaseSettings};
use nts::email_client::EmailClient;
use nts::startup::run;
use nts::telemetry::{get_subscriber, init_subscriber};
use once_cell::sync::Lazy;
use sqlx::{Connection, Executor, PgConnection, PgPool};
use std::net::TcpListener;
use uuid::Uuid;

static TRACING: Lazy<()> = Lazy::new(|| {
    let default_filter_level = "info".to_string();
    let subscriber_name = "test".to_string();
    if std::env::var("TEST_LOG").is_ok() {
        let subscriber = get_subscriber(subscriber_name, default_filter_level, std::io::stdout);
        init_subscriber(subscriber);
    } else {
        let subscriber = get_subscriber(subscriber_name, default_filter_level, std::io::sink);
        init_subscriber(subscriber);
    }
});

pub struct TestApp {
    pub address: String,
    pub db_pool: PgPool,
    database_settings: DatabaseSettings,
}

impl TestApp {
    pub async fn teardown_database(&self) {
        self.db_pool.close().await;

        let mut connection = PgConnection::connect(
            format!(
                "postgres://{}@{}:{}",
                &self.database_settings.username,
                &self.database_settings.host,
                self.database_settings.port
            )
            .as_str(),
        )
        .await
        .expect("Failed to connect to Postgres");

        connection
            .execute(
                format!(
                    r#"DROP DATABASE "{}";"#,
                    &self.database_settings.database_name
                )
                .as_str(),
            )
            .await
            .expect("Failed to drop database");
    }
}

pub async fn spawn_app() -> TestApp {
    Lazy::force(&TRACING);

    let listener = TcpListener::bind("127.0.0.1:0").expect("Failed to bind random port");
    let port = listener.local_addr().unwrap().port();
    let address = format!("http://127.0.0.1:{}", port);

    let mut configuration = get_configuration().expect("Failed to read configuration.");
    configuration.database.database_name = Uuid::new_v4().to_string();
    let connection_pool = configure_database(&configuration.database).await;

    let sender_email = configuration
        .email_client
        .sender()
        .expect("Invalid sender email address.");
    let timeout = configuration.email_client.timeout();
    let email_client = EmailClient::new(
        configuration.email_client.base_url,
        sender_email,
        configuration.email_client.authorization_token,
        timeout,
    );

    let server =
        run(listener, connection_pool.clone(), email_client).expect("Failed to bind address");
    let _ = tokio::spawn(server);

    TestApp {
        address,
        db_pool: connection_pool,
        database_settings: configuration.database,
    }
}

async fn configure_database(config: &DatabaseSettings) -> PgPool {
    let mut connection = PgConnection::connect_with(&config.without_db())
        .await
        .expect("Failed to connect to Postgres");
    connection
        .execute(format!(r#"CREATE DATABASE "{}";"#, config.database_name).as_str())
        .await
        .expect("Failed to create database");

    let connection_pool = PgPool::connect_with(config.with_db())
        .await
        .expect("Failed to connect to Postgres");

    // iterate over migration dir and execute query files
    let mut files: Vec<_> = Vec::new();
    let migration_dir = std::env::current_dir().unwrap().join("migrations");

    if let Ok(entries) = std::fs::read_dir(&migration_dir) {
        for entry in entries {
            if let Ok(entry) = entry {
                files.push(entry.file_name());
            } else {
                panic!("Failed to read directory entry");
            }
        }
    } else {
        panic!("Failed to read migration directory");
    }

    files.sort();

    for file in files.into_iter() {
        let query = std::fs::read_to_string(&migration_dir.join(file)).unwrap();
        connection_pool
            .execute(query.as_str())
            .await
            .expect("Failed to execute migration query");
    }

    connection_pool
}
