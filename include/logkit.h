/* LogKit.dll — structured, rolling, crash-safe logging.
 *
 * One format in every app, one record per line:
 *
 *   2026-10-23T19:50:43.123Z INFO  net: connected host=example.com attempt=2
 *
 * UTC ISO-8601 (millisecond precision), a fixed-width level, a
 * target (module/subsystem), the message, then structured
 * key=value fields. Messages are escaped onto one physical line:
 * an embedded newline becomes the two characters "\n".
 *
 * Crash-safe: every record is flushed as it is written, so a
 * crash loses at most the record being written. Rotation is
 * size-based and happens BEFORE a write that would overflow, so
 * a record never straddles two files: app.log, app.log.1, ...
 * up to `keep_files` backups (0 = rotation discards).
 *
 * A logger handle is thread-safe: writes are serialized inside
 * the library; any thread may log through it. Errors set a
 * thread-local lk_last_error() (message + code).
 */
#ifndef LOGKIT_H
#define LOGKIT_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#if defined(_WIN32) && defined(LK_BUILD)
#define LK_API __declspec(dllexport)
#elif defined(_WIN32)
#define LK_API __declspec(dllimport)
#else
#define LK_API
#endif

enum {
    LK_OK = 0,
    LK_ERR_INVALID_ARG = 1,
    LK_ERR_PARSE = 2,   /* fields_json is not a flat JSON object */
    LK_ERR_IO = 3,
    LK_ERR_INTERNAL = 4
};

/* Levels. Higher = more severe. */
enum {
    LK_TRACE = 1,
    LK_DEBUG = 2,
    LK_INFO = 3,
    LK_WARN = 4,
    LK_ERROR = 5
};

/* lk_open flag: sync every record to disk after writing. Covers
 * OS crashes too, at a real speed cost; process crashes are
 * already covered by the default flush-per-record. */
#define LK_SYNC_ON_WRITE 0x1

typedef struct LkLogger LkLogger;

LK_API const char *lk_version(void);
LK_API const char *lk_last_error(void);
LK_API int lk_last_error_code(void);
LK_API void lk_free_string(char *p);

/* Open a logger appending to `path` (parent directories and the
 * file are created as needed). Records below `min_level` are
 * dropped. `max_bytes` of 0 disables rotation. */
LK_API int lk_open(const char *path, int min_level,
                   uint64_t max_bytes, uint32_t keep_files,
                   uint32_t flags, LkLogger **out);
/* Flush and free the logger. */
LK_API void lk_close(LkLogger *logger);

/* Log one record. Returns LK_OK even when the record is filtered
 * out by the level floor. */
LK_API int lk_log(LkLogger *logger, int level, const char *target,
                  const char *message);
/* Log with structured fields: `fields_json` is a FLAT JSON
 * object ({"host":"example.com","attempt":2}) rendered as
 * key=value pairs. NULL means no fields. Malformed or nested
 * JSON fails with LK_ERR_PARSE and writes nothing. */
LK_API int lk_log_fields(LkLogger *logger, int level,
                         const char *target, const char *message,
                         const char *fields_json);

LK_API int lk_flush(LkLogger *logger);
LK_API int lk_set_level(LkLogger *logger, int level);
/* 1 = would be written, 0 = filtered, -1 = error. */
LK_API int lk_enabled(LkLogger *logger, int level);

/* Convenience macros. */
#define LK_TRACE(lg, target, msg) lk_log((lg), LK_TRACE, (target), (msg))
#define LK_DEBUG(lg, target, msg) lk_log((lg), LK_DEBUG, (target), (msg))
#define LK_INFO(lg, target, msg)  lk_log((lg), LK_INFO, (target), (msg))
#define LK_WARN(lg, target, msg)  lk_log((lg), LK_WARN, (target), (msg))
#define LK_ERROR(lg, target, msg) lk_log((lg), LK_ERROR, (target), (msg))

#ifdef __cplusplus
}
#endif
#endif /* LOGKIT_H */
