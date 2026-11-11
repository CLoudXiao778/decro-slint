#ifndef NE_CLI_CMDS_H
#define NE_CLI_CMDS_H
/* Command entry points, one per family; called by main()'s dispatcher. Each
 * lives in its own src/cli/cmd_*.c translation unit. */

/* qr */
int cmd_qr_key(void);
int cmd_qr_check(const char *unikey);

/* song (play URL / entitlement / purchased singles) */
int cmd_check_music(const char *id);
int cmd_song_url(const char *id, const char *level_in);
int cmd_song_download_url(const char *id, const char *level_in);
int cmd_song_music_quality(const char *id);
int cmd_song_purchased(const char *limit_in, const char *offset_in);
int cmd_check_quality(const char *id, const char *level);

/* album (digital albums) */
int cmd_album_purchased(const char *limit_in, const char *offset_in);
int cmd_album(const char *id);

/* user / account / liked / playlist display */
int cmd_liked(void);
int cmd_liked_check(const char *song_id);
int cmd_playlists(void);
int cmd_playlist_cover(const char *id);
int cmd_lyric(const char *id);
int cmd_playlist_tracks(const char *id);
int cmd_account_name(void);
int cmd_account_info(void);

/* write (favorite / playlist mutations) */
int cmd_like(const char *id, const char *like);
int cmd_subscribe(const char *id, const char *t);
int cmd_track(const char *op, const char *pid, const char *sid);

#endif