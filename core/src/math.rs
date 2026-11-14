pub type Vector = [f32; 3];
pub type Quaternion = [f32; 4];
pub type Matrix = [f32; 16];

pub const IDENTITY_QUATERNION: Quaternion = [0.0, 0.0, 0.0, 1.0];
pub const IDENTITY_MATRIX: Matrix = [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0];

pub fn normalize(vector: Vector) -> Vector {
    let length = (vector[0] * vector[0] + vector[1] * vector[1] + vector[2] * vector[2]).sqrt();
    if length == 0.0 {
        return vector;
    }
    [vector[0] / length, vector[1] / length, vector[2] / length]
}

pub fn quaternion_normalize(quaternion: Quaternion) -> Quaternion {
    let length = (quaternion[0] * quaternion[0] + quaternion[1] * quaternion[1] + quaternion[2] * quaternion[2] + quaternion[3] * quaternion[3]).sqrt();
    if length == 0.0 {
        return IDENTITY_QUATERNION;
    }
    [quaternion[0] / length, quaternion[1] / length, quaternion[2] / length, quaternion[3] / length]
}

pub fn quaternion_multiply(a: Quaternion, b: Quaternion) -> Quaternion {
    [
        a[3] * b[0] + a[0] * b[3] + a[1] * b[2] - a[2] * b[1],
        a[3] * b[1] - a[0] * b[2] + a[1] * b[3] + a[2] * b[0],
        a[3] * b[2] + a[0] * b[1] - a[1] * b[0] + a[2] * b[3],
        a[3] * b[3] - a[0] * b[0] - a[1] * b[1] - a[2] * b[2],
    ]
}

pub fn quaternion_rotate(quaternion: Quaternion, vector: Vector) -> Vector {
    let [x, y, z, w] = quaternion;
    let twice_cross = [2.0 * (y * vector[2] - z * vector[1]), 2.0 * (z * vector[0] - x * vector[2]), 2.0 * (x * vector[1] - y * vector[0])];
    [
        vector[0] + w * twice_cross[0] + (y * twice_cross[2] - z * twice_cross[1]),
        vector[1] + w * twice_cross[1] + (z * twice_cross[0] - x * twice_cross[2]),
        vector[2] + w * twice_cross[2] + (x * twice_cross[1] - y * twice_cross[0]),
    ]
}

pub fn quaternion_from_to(from: Vector, to: Vector) -> Quaternion {
    let a = normalize(from);
    let b = normalize(to);
    let cosine = a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
    if cosine < -1.0 + 1e-6 {
        let helper = if a[0].abs() < 0.9 { [1.0, 0.0, 0.0] } else { [0.0, 1.0, 0.0] };
        let axis = normalize([a[1] * helper[2] - a[2] * helper[1], a[2] * helper[0] - a[0] * helper[2], a[0] * helper[1] - a[1] * helper[0]]);
        return [axis[0], axis[1], axis[2], 0.0];
    }
    quaternion_normalize([a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0], 1.0 + cosine])
}

pub fn quaternion_slerp(a: Quaternion, b: Quaternion, amount: f32) -> Quaternion {
    if amount >= 1.0 {
        return b;
    }
    if amount <= 0.0 {
        return a;
    }
    let mut cosine = a[0] * b[0] + a[1] * b[1] + a[2] * b[2] + a[3] * b[3];
    let mut end = b;
    if cosine < 0.0 {
        cosine = -cosine;
        end = [-b[0], -b[1], -b[2], -b[3]];
    }
    let angle = cosine.min(1.0).acos();
    let sine = angle.sin();
    let (weight_a, weight_b) = if sine < 1e-6 { (1.0 - amount, amount) } else { (((1.0 - amount) * angle).sin() / sine, (amount * angle).sin() / sine) };
    quaternion_normalize([
        a[0] * weight_a + end[0] * weight_b,
        a[1] * weight_a + end[1] * weight_b,
        a[2] * weight_a + end[2] * weight_b,
        a[3] * weight_a + end[3] * weight_b,
    ])
}

pub fn quaternion_power(quaternion: Quaternion, exponent: f32) -> Quaternion {
    let sign = if quaternion[3] < 0.0 { -1.0 } else { 1.0 };
    let [x, y, z, w] = [quaternion[0] * sign, quaternion[1] * sign, quaternion[2] * sign, quaternion[3] * sign];
    let sine_half = (x * x + y * y + z * z).sqrt();
    if sine_half == 0.0 {
        return IDENTITY_QUATERNION;
    }
    let half_angle = sine_half.atan2(w) * exponent;
    let factor = half_angle.sin() / sine_half;
    [x * factor, y * factor, z * factor, half_angle.cos()]
}

pub fn quaternion_from_euler(yaw: f32, pitch: f32, roll: f32) -> Quaternion {
    let around_y = [0.0, (yaw * 0.5).sin(), 0.0, (yaw * 0.5).cos()];
    let around_x = [(pitch * 0.5).sin(), 0.0, 0.0, (pitch * 0.5).cos()];
    let around_z = [0.0, 0.0, (roll * 0.5).sin(), (roll * 0.5).cos()];
    quaternion_multiply(quaternion_multiply(around_y, around_x), around_z)
}

pub fn matrix_compose(translation: Vector, rotation: Quaternion, scale: Vector) -> Matrix {
    let [x, y, z, w] = rotation;
    [
        (1.0 - 2.0 * (y * y + z * z)) * scale[0],
        2.0 * (x * y + w * z) * scale[0],
        2.0 * (x * z - w * y) * scale[0],
        0.0,
        2.0 * (x * y - w * z) * scale[1],
        (1.0 - 2.0 * (x * x + z * z)) * scale[1],
        2.0 * (y * z + w * x) * scale[1],
        0.0,
        2.0 * (x * z + w * y) * scale[2],
        2.0 * (y * z - w * x) * scale[2],
        (1.0 - 2.0 * (x * x + y * y)) * scale[2],
        0.0,
        translation[0],
        translation[1],
        translation[2],
        1.0,
    ]
}

pub fn matrix_multiply(a: &Matrix, b: &Matrix) -> Matrix {
    let mut result = [0.0; 16];
    for column in 0..4 {
        for row in 0..4 {
            result[column * 4 + row] = a[row] * b[column * 4] + a[4 + row] * b[column * 4 + 1] + a[8 + row] * b[column * 4 + 2] + a[12 + row] * b[column * 4 + 3];
        }
    }
    result
}

pub fn matrix_inverse(matrix: &Matrix) -> Matrix {
    let [a00, a10, a20, _, a01, a11, a21, _, a02, a12, a22, _, t0, t1, t2, _] = *matrix;
    let cofactor00 = a11 * a22 - a12 * a21;
    let cofactor01 = a12 * a20 - a10 * a22;
    let cofactor02 = a10 * a21 - a11 * a20;
    let determinant = a00 * cofactor00 + a01 * cofactor01 + a02 * cofactor02;
    if determinant == 0.0 {
        return IDENTITY_MATRIX;
    }
    let inverse = 1.0 / determinant;
    let mut result = [
        cofactor00 * inverse,
        cofactor01 * inverse,
        cofactor02 * inverse,
        0.0,
        (a02 * a21 - a01 * a22) * inverse,
        (a00 * a22 - a02 * a20) * inverse,
        (a01 * a20 - a00 * a21) * inverse,
        0.0,
        (a01 * a12 - a02 * a11) * inverse,
        (a02 * a10 - a00 * a12) * inverse,
        (a00 * a11 - a01 * a10) * inverse,
        0.0,
        0.0,
        0.0,
        0.0,
        1.0,
    ];
    for row in 0..3 {
        result[12 + row] = -(result[row] * t0 + result[4 + row] * t1 + result[8 + row] * t2);
    }
    result
}

pub fn transform_point(matrix: &Matrix, point: Vector) -> Vector {
    [
        matrix[0] * point[0] + matrix[4] * point[1] + matrix[8] * point[2] + matrix[12],
        matrix[1] * point[0] + matrix[5] * point[1] + matrix[9] * point[2] + matrix[13],
        matrix[2] * point[0] + matrix[6] * point[1] + matrix[10] * point[2] + matrix[14],
    ]
}

pub fn matrix_rotation(matrix: &Matrix) -> Quaternion {
    let x_axis = normalize([matrix[0], matrix[1], matrix[2]]);
    let y_axis = normalize([matrix[4], matrix[5], matrix[6]]);
    let z_axis = normalize([matrix[8], matrix[9], matrix[10]]);
    let (r00, r10, r20) = (x_axis[0], x_axis[1], x_axis[2]);
    let (r01, r11, r21) = (y_axis[0], y_axis[1], y_axis[2]);
    let (r02, r12, r22) = (z_axis[0], z_axis[1], z_axis[2]);
    let trace = r00 + r11 + r22;
    let quaternion = if trace > 0.0 {
        let scale = (trace + 1.0).sqrt() * 2.0;
        [(r21 - r12) / scale, (r02 - r20) / scale, (r10 - r01) / scale, 0.25 * scale]
    } else if r00 > r11 && r00 > r22 {
        let scale = (1.0 + r00 - r11 - r22).sqrt() * 2.0;
        [0.25 * scale, (r01 + r10) / scale, (r02 + r20) / scale, (r21 - r12) / scale]
    } else if r11 > r22 {
        let scale = (1.0 + r11 - r00 - r22).sqrt() * 2.0;
        [(r01 + r10) / scale, 0.25 * scale, (r12 + r21) / scale, (r02 - r20) / scale]
    } else {
        let scale = (1.0 + r22 - r00 - r11).sqrt() * 2.0;
        [(r02 + r20) / scale, (r12 + r21) / scale, 0.25 * scale, (r10 - r01) / scale]
    };
    quaternion_normalize(quaternion)
}
