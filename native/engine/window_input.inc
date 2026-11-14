bool window_input(window* w, float* pan_x, float* pan_y, float* zoom, float* cursor_x, float* cursor_y, float* turn, int* keys)
{
    *turn = (float)(w->turn_ccw - w->turn_cw);
    bool moved = w->pan_x != 0 || w->pan_y != 0 || w->zoom != 1 || *turn != 0 || w->keys != 0;
    *keys = w->keys & 3;
    if (w->keys & 4)
    {
        w->turn_ccw = false;
    }
    if (w->keys & 8)
    {
        w->turn_cw = false;
    }
    w->keys = 0;
    *pan_x = w->pan_x;
    *pan_y = w->pan_y;
    *zoom = w->zoom;
    *cursor_x = w->cursor_x;
    *cursor_y = w->cursor_y;
    w->pan_x = 0;
    w->pan_y = 0;
    w->zoom = 1;
    return moved;
}
