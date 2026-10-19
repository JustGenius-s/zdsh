#[derive(Debug)]
pub enum CustomImageFormat {
    Rgb,
    Rgba,
}

impl CustomImageFormat {
    fn create_tag(&self) -> String {
        match self {
            CustomImageFormat::Rgb => "rgb".into(),
            CustomImageFormat::Rgba => "rgba".into(),
        }
    }
}

#[derive(Debug, Clone)]
pub enum CustomHeaderCreationError {
    ExpectedDataSizeMismatch {
        expected_bytes: usize,
        actual_bytes: usize,
    },
}

#[derive(Debug)]
pub struct CustomImageHeader {
    pub width: u32,
    pub height: u32,
    pub image_format: CustomImageFormat,
}

impl CustomImageHeader {
    pub fn create_header(&self) -> String {
        format!(
            "warp-img:{}:{}:{}:",
            self.image_format.create_tag(),
            self.width,
            self.height
        )
    }

    pub fn prepend_custom_header(
        mut data: Vec<u8>,
        width: u32,
        height: u32,
        image_format: CustomImageFormat,
    ) -> Result<Vec<u8>, CustomHeaderCreationError> {
        let bytes_per_pixel = match image_format {
            CustomImageFormat::Rgb => 3,
            CustomImageFormat::Rgba => 4,
        };
        let expected_byte_count = (bytes_per_pixel * width * height) as usize;

        if expected_byte_count != data.len() {
            return Err(CustomHeaderCreationError::ExpectedDataSizeMismatch {
                expected_bytes: expected_byte_count,
                actual_bytes: data.len(),
            });
        }

        let custom_header = CustomImageHeader {
            width,
            height,
            image_format,
        };

        data.splice(
            0..0,
            custom_header.create_header().as_bytes().iter().copied(),
        );

        Ok(data)
    }
}
pub enum FitType {
    /// Expands the image to fill the entire space while maintaining the aspect ratio.
    Cover,
    /// Resizes the image to maximum size that fully fits in the given bounds,
    /// maintaining the aspect ratio.
    Contain,
    /// Stretches the image to fit the given bounds, ignoring the aspect ratio.
    /// This should likely only be used with SVGs, and not all SVGs are designed
    /// to be stretched.
    Stretch,
}

impl FitType {
    pub fn should_retain_aspect_ratio(&self) -> bool {
        match self {
            FitType::Cover | FitType::Contain => true,
            FitType::Stretch => false,
        }
    }
}

pub fn resize_dimensions(
    width: u32,
    height: u32,
    nwidth: u32,
    nheight: u32,
    fit_type: FitType,
) -> (u32, u32) {
    use std::cmp::max;

    let wratio = nwidth as f64 / width as f64;
    let hratio = nheight as f64 / height as f64;

    let ratio = match fit_type {
        FitType::Cover => f64::max(wratio, hratio),
        FitType::Contain => {
            // Resize the image, maintaining aspect ratio, such that the larger
            // dimension equals its bounds.
            f64::min(wratio, hratio)
        }
        // Stretch doesn't maintain the aspect ratio
        FitType::Stretch => return (nwidth, nheight),
    };

    let nw = max((width as f64 * ratio).round() as u64, 1);
    let nh = max((height as f64 * ratio).round() as u64, 1);

    if nw > u64::from(u32::MAX) {
        let ratio = u32::MAX as f64 / width as f64;
        (u32::MAX, max((height as f64 * ratio).round() as u32, 1))
    } else if nh > u64::from(u32::MAX) {
        let ratio = u32::MAX as f64 / height as f64;
        (max((width as f64 * ratio).round() as u32, 1), u32::MAX)
    } else {
        (nw as u32, nh as u32)
    }
}
pub fn parse_u32(input: &[u8]) -> Option<u32> {
    if input.is_empty() {
        return None;
    }
    let mut num: u32 = 0;
    for c in input {
        let c = *c as char;
        let digit = c.to_digit(10)?;
        num = num.checked_mul(10).and_then(|v| v.checked_add(digit))?;
    }
    Some(num)
}

pub fn parse_i32(input: &[u8]) -> Option<i32> {
    if input.is_empty() {
        return None;
    }

    let mut negative = false;
    let mut input = input;

    if input[0] == b'-' {
        negative = true;
        input = &input[1..];

        if input.is_empty() {
            return None;
        }
    }

    let mut num: i32 = 0;
    for c in input {
        let c = *c as char;
        let digit = c.to_digit(10)?;
        num = num
            .checked_mul(10)
            .and_then(|v| v.checked_add(digit as i32))?;
    }

    if negative {
        num = num.checked_neg()?;
    }

    Some(num)
}
