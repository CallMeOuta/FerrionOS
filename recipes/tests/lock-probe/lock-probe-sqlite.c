#define _GNU_SOURCE
#include <errno.h>
#include <sqlite3.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static char db_path[128];
static const char *mode;

static int fail(sqlite3 *db, const char *what, int writer, int step) {
    printf("sqlite-%s: writer %d step %d %s: %s (extended %d, errno %d %s)\n", mode, writer, step, what,
           sqlite3_errmsg(db), sqlite3_extended_errcode(db), sqlite3_system_errno(db),
           strerror(sqlite3_system_errno(db)));
    fflush(stdout);
    return 1;
}

static int writer(int id, int count) {
    sqlite3 *db;
    if (sqlite3_open(db_path, &db) != SQLITE_OK) {
        return fail(db, "open", id, 0);
    }
    sqlite3_extended_result_codes(db, 1);
    sqlite3_busy_timeout(db, 20000);
    char sql[160];
    for (int i = 1; i <= count; i++) {
        snprintf(sql, sizeof sql,
                 "BEGIN IMMEDIATE; UPDATE c SET n = n + 1; INSERT INTO t VALUES(%d, %d); COMMIT;", id, i);
        if (sqlite3_exec(db, sql, NULL, NULL, NULL) != SQLITE_OK) {
            return fail(db, "transaction", id, i);
        }
    }
    return sqlite3_close(db) == SQLITE_OK ? 0 : fail(db, "close", id, count);
}

static int append(void *out, int columns, char **values, char **names) {
    (void)names;
    for (int i = 0; i < columns; i++) {
        strncat(out, values[i] ? values[i] : "NULL", 255 - strlen(out));
        strncat(out, " ", 255 - strlen(out));
    }
    return 0;
}

static int create_db(void) {
    sqlite3 *db;
    char other[160];
    unlink(db_path);
    snprintf(other, sizeof other, "%s-wal", db_path);
    unlink(other);
    snprintf(other, sizeof other, "%s-shm", db_path);
    unlink(other);
    if (sqlite3_open(db_path, &db) != SQLITE_OK) {
        return fail(db, "open", 0, 0);
    }
    sqlite3_extended_result_codes(db, 1);
    if (sqlite3_exec(db,
                     "PRAGMA page_size = 32768; PRAGMA journal_mode = wal; CREATE TABLE r(i INTEGER, v TEXT);",
                     NULL, NULL, NULL) != SQLITE_OK) {
        return fail(db, "create", 0, 0);
    }
    sqlite3_close(db);
    pid_t pids[2];
    for (int w = 0; w < 2; w++) {
        pids[w] = fork();
        if (pids[w] == 0) {
            char sql[256];
            snprintf(sql, sizeof sql,
                     "PRAGMA cache_size = -2048; BEGIN IMMEDIATE; "
                     "WITH RECURSIVE s(i) AS (SELECT %d UNION ALL SELECT i + 1 FROM s WHERE i < %d) "
                     "INSERT INTO r SELECT i, printf('row %%d', i) FROM s; COMMIT;",
                     w * 50 + 1, w * 50 + 50);
            if (sqlite3_open(db_path, &db) != SQLITE_OK) {
                _exit(fail(db, "open", w + 1, 0));
            }
            sqlite3_extended_result_codes(db, 1);
            sqlite3_busy_timeout(db, 20000);
            if (sqlite3_exec(db, sql, NULL, NULL, NULL) != SQLITE_OK) {
                _exit(fail(db, "insert", w + 1, 0));
            }
            _exit(sqlite3_close(db) == SQLITE_OK ? 0 : fail(db, "close", w + 1, 0));
        }
    }
    int failed = 0;
    for (int w = 0; w < 2; w++) {
        int raw = 0;
        waitpid(pids[w], &raw, 0);
        failed |= !WIFEXITED(raw) || WEXITSTATUS(raw) != 0;
    }
    printf("sqlite-create: %s\n", failed ? "a writer failed" : "two writer processes wrote 100 rows");
    return failed;
}

static int read_db(void) {
    sqlite3 *db;
    char summary[256] = "";
    if (sqlite3_open_v2(db_path, &db, SQLITE_OPEN_READONLY, NULL) != SQLITE_OK) {
        return fail(db, "open", 0, 0);
    }
    sqlite3_extended_result_codes(db, 1);
    if (sqlite3_exec(db, "PRAGMA cache_size = -2048; SELECT count(*), sum(i) FROM r;", append, summary, NULL) !=
        SQLITE_OK) {
        return fail(db, "read", 0, 0);
    }
    sqlite3_close(db);
    printf("sqlite-read: rows %s\n", summary);
    return 0;
}

int main(int argc, char **argv) {
    if (argc < 2) {
        fprintf(stderr, "usage: lock-probe-sqlite wal|delete [transactions]\n       lock-probe-sqlite create|read path\n");
        return 2;
    }
    mode = argv[1];
    if (strcmp(mode, "create") == 0 || strcmp(mode, "read") == 0) {
        if (argc < 3 || snprintf(db_path, sizeof db_path, "%s", argv[2]) >= (int)sizeof db_path) {
            fprintf(stderr, "lock-probe-sqlite: %s needs a path shorter than %zu bytes\n", mode, sizeof db_path);
            return 2;
        }
        return mode[0] == 'c' ? create_db() : read_db();
    }
    int count = argc > 2 ? atoi(argv[2]) : 100;
    snprintf(db_path, sizeof db_path, "/tmp/lock-probe-sqlite-%s-%d.db", mode, (int)getpid());
    sqlite3 *db;
    char sql[160];
    snprintf(sql, sizeof sql,
             "PRAGMA journal_mode=%s; CREATE TABLE t(writer INTEGER, i INTEGER); CREATE TABLE c(n INTEGER); "
             "INSERT INTO c VALUES(0);",
             mode);
    if (sqlite3_open(db_path, &db) != SQLITE_OK || sqlite3_exec(db, sql, NULL, NULL, NULL) != SQLITE_OK) {
        return fail(db, "setup", 0, 0);
    }
    sqlite3_close(db);

    struct timespec start, end;
    clock_gettime(CLOCK_MONOTONIC, &start);
    fflush(stdout);
    pid_t pids[2];
    for (int w = 0; w < 2; w++) {
        pids[w] = fork();
        if (pids[w] == 0) {
            _exit(writer(w + 1, count));
        }
    }
    int status[2];
    for (int w = 0; w < 2; w++) {
        int raw = 0;
        waitpid(pids[w], &raw, 0);
        status[w] = WIFEXITED(raw) ? WEXITSTATUS(raw) : 128 + WTERMSIG(raw);
    }
    clock_gettime(CLOCK_MONOTONIC, &end);

    char journal[256] = "";
    char summary[256] = "";
    sqlite3_open(db_path, &db);
    sqlite3_extended_result_codes(db, 1);
    if (sqlite3_exec(db, "PRAGMA journal_mode;", append, journal, NULL) != SQLITE_OK ||
        sqlite3_exec(db,
                     "PRAGMA integrity_check; SELECT n FROM c; SELECT count(*) FROM t; "
                     "SELECT count(DISTINCT writer || '-' || i) FROM t;",
                     append, summary, NULL) != SQLITE_OK) {
        fail(db, "summary", 0, 0);
    }
    sqlite3_close(db);
    journal[strcspn(journal, " ")] = 0;
    printf("sqlite-%s: journal %s seconds %ld\n", mode, journal, (long)(end.tv_sec - start.tv_sec));
    printf("sqlite-%s: status %d %d result %s\n", mode, status[0], status[1], summary);
    char other[160];
    unlink(db_path);
    snprintf(other, sizeof other, "%s-journal", db_path);
    unlink(other);
    snprintf(other, sizeof other, "%s-wal", db_path);
    unlink(other);
    snprintf(other, sizeof other, "%s-shm", db_path);
    unlink(other);
    return status[0] || status[1];
}
