# getting started

this page walks through installing hikari vrm, running it in a window or headless, and the http api. the [readme](../README.md) has the numbers and licensing.

## install

extract the zip for your os from the [releases page](https://github.com/levinkin/hikari-vrm/releases). no compiler or sdk needed. put your vrm 1.0 models (`.vrm`) in `models/`.

the binaries aren't signed:
- mac: run it once, then System Settings > Privacy & Security > Open Anyway.
- windows: smartscreen > more info > run anyway.
- linux: needs a vulkan driver and x11.

## running it

double-click `hikari-vrm` in the folder (`hikari-vrm.exe` on windows), or run it from inside the folder:

```
./hikari-vrm
```

it opens a window and serves the http api on port 8794. default model is the first model by name if unspecified (`model` in `settings.json`). vrm 0.x models need re-exporting as vrm 1.0.

## window

| control | what it does |
|---|---|
| drag | moves the model |
| scroll / pinch | zooms around the cursor |
| q / e (hold) | rotates the model while the window is focused |
| space | resets to the model's default framing |
| ctrl+s (cmd+s on mac) | saves the current framing for that model (`views.json`) |
| alt+enter (option+return on mac) | fullscreen |
| esc | leaves fullscreen, or quits |
| cmd+q (mac) | quits |

framing doesn't save on its own. everything you do in the window also moves the http streams for that model.

## headless

`./hikari-vrm --headless` skips the window and only runs the http server. `GET /listen` holds the connection open and sends `fps=` frames a second (60 by default) as a chunked `application/octet-stream`, one frame per chunk.

frame contents, all big-endian:
- 4-byte timestamp in ms
- 4-byte data length (codec byte + picture)
- 4 bytes of head pose (yaw and pitch as signed 16-bit numbers, for servos)
- codec byte and the picture

## actions

| endpoint | params (type, default) | what it does | returns | example |
|---|---|---|---|---|
| `GET /listen` | `codec` (string, `jpeg`), `fps` (integer, `60`), `size` (WxH, `size` setting) | streams idling until you close the connection | frame stream | `GET /listen?codec=qoi&fps=30` |
| `POST /talk` | `codec`, `fps`, `size` (same as `/listen`). body: wav | lip syncs to the wav audio | frame stream | `POST /talk?codec=qoi&size=320x240` with a wav body |
| `POST /state` | `face` (string), `pose` (string), `head_lead_ms` (integer 0 to 1500). leave one out to keep it | sets a face/pose and holds it | `{"face", "pose", "asleep", "head_lead_ms"}`, `400` for an unknown state | `POST /state?face=happy` |
| `GET /state` | none | the current face/pose and every state this model has | `{"face", "pose", "asleep", "head_lead_ms", "states": [...]}` | - |
| `POST /action` | `action` (string, required) | plays an action clip or flashes an expression, then returns to idle | `{"action", "motion", "expression"}`, `400` for an unknown action | `POST /action?action=look_left` |
| `GET /action` | none | every action this model has, and the idle ones | `{"actions": [...], "idle": [...]}` | - |
| `POST /params` | none. body: json | holds params on top of idle and actions until changed. `null` lets one go | every held param, `400` naming unknown params | `{"ParamAngleX": 10.0, "ParamLeftUpperArmZ": 110}` |

actions are the `.vrma` clips in `actions/` and `animations/`, named by file name. a clip in `animations/` replaces an action with the same name. default shipped ones: `dance`, `double_blink`, `glance_left`, `glance_right`, `look_ahead`, `look_around`, `look_down`, `look_left`, `look_right`, `look_up`, `look_up_think`, `sleep`, `tilt_smile` and `yawn`. an action only moves the bones it has tracks for. `sleep` holds until the next action, `/talk` or `/state`.

## misc endpoints

| endpoint | params (type, default) | what it does | returns | example |
|---|---|---|---|---|
| `GET /models` | none | lists the models it found and the current one | `{"model", "models": [...]}` | - |
| `GET /view` | none | the model's current framing, its saved one and its default | `{"zoom", "x", "y", "rot", "saved", "default"}` (`saved` is `null` if never saved) | - |
| `PUT /view` | `zoom`, `x`, `y`, `rot` (number, current value), `save` (bool, `false`) | moves the framing live. `save=true` also writes `views.json` | `204` | `PUT /view?zoom=1.5&save=true` |
| `GET /settings` | none | the current settings | json | - |
| `PUT /settings` | json body with any of `model`, `face`, `idle_actions`, `action_every`, `rest_angle` | changes them live and saves `settings.json` | the new settings, `400` for a bad value or a restart-only key | `{"face": "neutral", "action_every": [10, 30]}` |
| `GET /background` | none | describes the current background | `{"kind", "name", "updated", ...}` | - |
| `GET /background/file` | none | the background image itself | the image, or `404` | - |
| `PUT /background` | `name` (string, `""`). body: png, jpeg or gif | sets a background (5 MB max, animated gifs play) | `200`, `400` if it isn't png/jpeg/gif, `413` if too big | `PUT /background?name=room.png` with a png body |
| `DELETE /background` | none | removes the background | `200` | - |

## params

| param | type | accepts | default |
|---|---|---|---|
| `codec` | string | `jpeg`, `pal256`, `rgb565`, `split`, `qoi`, `mjpeg` (plain multipart jpeg, no frame header) | `jpeg` |
| `fps` | integer | 1 to 240 | `60` |
| `size` | WxH | e.g. `160x120`, up to `3840x2160`. ignored for `split` | the `size` setting |
| `zoom`, `x`, `y`, `rot` | number | `PUT /view` only | the model's current value |
| `face`, `pose` | string | any state from `GET /state`, `none` to clear | the `face` setting, `none` |
| `action` | string | any action from `GET /action` | required |

`POST /params` takes the face params (`ParamAngleX/Y/Z`, `ParamBodyAngleX`, `ParamEyeLOpen`, `ParamEyeROpen`, `ParamEyeBallX/Y`, `ParamMouthOpenY`, `ParamMouthForm`, `ParamBreath`), one param per expression named like the expression, and three per humanoid bone the model has, named `Param<Bone><Axis>`. bone values are degrees in the normalized bone space on top of the idle pose. `X` turns around the left-right axis, `Y` around the up axis, `Z` around the front-back axis, combined `Y` first, then `X`, then `Z`.

| param | bone |
|---|---|
| `ParamHipsX/Y/Z`, `ParamSpineX/Y/Z`, `ParamChestX/Y/Z`, `ParamUpperChestX/Y/Z`, `ParamNeckX/Y/Z`, `ParamHeadX/Y/Z`, `ParamJawX/Y/Z` | torso and head |
| `ParamLeftEyeX/Y/Z`, `ParamRightEyeX/Y/Z` | eyes, added to look-at |
| `ParamLeftShoulderX/Y/Z`, `ParamLeftUpperArmX/Y/Z`, `ParamLeftLowerArmX/Y/Z`, `ParamLeftHandX/Y/Z` | left arm, `Right` for the right arm |
| `ParamLeftUpperLegX/Y/Z`, `ParamLeftLowerLegX/Y/Z`, `ParamLeftFootX/Y/Z`, `ParamLeftToesX/Y/Z` | left leg, `Right` for the right leg |
| `ParamLeftThumbMetacarpalX/Y/Z`, `ParamLeftThumbProximalX/Y/Z`, `ParamLeftThumbDistalX/Y/Z` | left thumb, `Right` for the right hand |
| `ParamLeft<Finger>ProximalX/Y/Z`, `ParamLeft<Finger>IntermediateX/Y/Z`, `ParamLeft<Finger>DistalX/Y/Z` | `<Finger>` is `Index`, `Middle`, `Ring` or `Little` |
| `ParamHipsPositionX/Y/Z` | hips offset in meters |

the relaxed idle pose already holds `ParamLeftUpperArmZ` at -70 degrees, so `ParamLeftUpperArmZ: 110` raises the left arm above the shoulder:

```sh
curl -X POST localhost:8794/params -d '{"ParamLeftUpperArmZ": 110, "ParamLeftLowerArmY": 30, "ParamHipsPositionY": -0.05}'
```

bools take `true` or `false`. anything else, or the same param twice, returns `400`.

## states, actions and loops

- states hold until you change them. they're the model's own vrm expressions, listed by `GET /state`.
- actions play once: the `.vrma` clips and the model's expressions. `idle_actions` picks one at random every `action_every` seconds.
- loops run underneath everything. `animations/idle.vrma` loops as the idle pose when it exists. `idle_animation` loops another clip instead, like `"idle_animation": "wave"` for `animations/wave.vrma`. with no idle clip the model keeps the built-in relaxed pose with breathing. a clip that fails to load, or an `idle_animation` that names no loaded clip, prints one line naming it and is skipped.

the `/talk` body is a 16-bit pcm wav, sent with `content-length` or chunked. she starts moving as the audio arrives.

in the frame stream, the picture after the codec byte depends on the codec: `jpeg` (byte 0) a jpeg, `pal256` (byte 1) a 512-byte rgb565 palette then one index per pixel as a raw lz4 block, `rgb565` (byte 2) little-endian pixels as a raw lz4 block, `split` (byte 3) top and bottom halves as two jpegs behind a 4-byte length of the top one, `qoi` (byte 4) a qoi image.

## command line

`./hikari-vrm` with no args opens the window with defaults.

| arg | type | what it does | default |
|---|---|---|---|
| `--window` | bool | opens the window | on |
| `--headless` | bool | no window, http server only | off |
| `--fps N` | integer | window frame rate, 1 to 240 | `60` |
| `--no-share` | bool | stops sharing frames with obs (spout2 on windows, syphon on mac) | off |
| `--port N` | integer | serves on that port for this run, 1 to 65535 | `port` setting |

## settings.json

| key | type | accepts | default |
|---|---|---|---|
| `model` | string | a `.vrm` file name in `models/`. switches live | the first model by name |
| `face` | string | a state name | `neutral` |
| `port` | integer | 1 to 65535 | `8794` |
| `size` | `[int, int]` | `[width, height]` the window opens at, and for streams without `size=`. up to `[3840, 2160]` | `[960, 720]` |
| `idle_actions` | list of strings | actions to pick from at random, `[]` for none. `sleep` is not allowed | `["look_around", "glance_left", "glance_right", "double_blink", "tilt_smile", "look_up_think"]` |
| `action_every` | integer or `[min, max]` | whole seconds between idle actions. a pair waits a random time between the two | `15` |
| `idle_loop` | string | an action to loop under everything | `""` |
| `idle_animation` | string or null | a clip to loop as the idle pose | `idle` |
| `rest_angle` | integer | 0 to 100, how far the robot's head tilts at rest (percent of its range) | `10` |

`port`, `size` and `idle_loop` need a restart. one renderer runs one model. for more models, run a copy per model on its own port (`--port`).
