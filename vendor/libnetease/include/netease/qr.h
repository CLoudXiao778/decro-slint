/* login_qr_service.go port — GetKey / CheckQR */
#ifndef NE_QR_H
#define NE_QR_H
#include <stddef.h>

/* returns malloc'd unikey ("" on failure), fills *code_out / *body_out
 * (body malloc'd, caller frees) */
char *ne_qr_get_key(double *code_out, char **body_out, size_t *body_len);

/* returns malloc'd response (body), fills *code_out */
char *ne_qr_check(const char *unikey, double *code_out, size_t *body_len);

/* qrcodeUrl = "http://music.163.com/login?codekey=<key>&chainId=<chain>" */
char *ne_qr_build_url(const char *unikey);
#endif
