// Copyright 2016 Joe Wilm, The Alacritty Project Contributors
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.
use font::Metrics;

use crate::index::Point;
use crate::term::cell::Flags;
use crate::term::color::Rgb;
use crate::term::{RenderableCell, SizeInfo};

#[derive(Debug, Copy, Clone)]
pub struct Rect<T> {
    pub x: T,
    pub y: T,
    pub width: T,
    pub height: T,
}

impl<T> Rect<T> {
    pub fn new(x: T, y: T, width: T, height: T) -> Self {
        Rect { x, y, width, height }
    }
}

#[derive(Debug)]
struct Line {
    flag: Flags,
    range: Option<(RenderableCell, Point)>,
}

impl Line {
    fn new(flag: Flags) -> Self {
        Self { flag, range: None }
    }
}

/// Rects for underline, strikeout and more.
pub struct Rects {
    inner: Vec<(Rect<f32>, Rgb)>,
    active_lines: Vec<Line>,
    metrics: Metrics,
    size: SizeInfo,
}

impl Rects {
    pub fn new(metrics: &Metrics, size: &SizeInfo) -> Self {
        let active_lines = vec![Line::new(Flags::UNDERLINE), Line::new(Flags::STRIKEOUT)];
        Self { inner: Vec::new(), active_lines, metrics: metrics.clone(), size: size.clone() }
    }

    /// Convert the stored rects to rectangles for the renderer.
    pub fn rects(&self) -> &Vec<(Rect<f32>, Rgb)> {
        &self.inner
    }

    pub fn size(&self) -> &SizeInfo {
        &self.size
    }

    pub fn metrics(&self) -> &Metrics {
        &self.metrics
    }

    /// Update the stored lines with the next cell info.
    pub fn update_lines(&mut self, size_info: &SizeInfo, cell: &RenderableCell, offset: (f32, f32)) {
        for line in self.active_lines.iter_mut() {
            match line.range {
                // Check for end if line is present
                Some((ref mut start, ref mut end)) => {
                    // No change in line
                    if cell.line == start.line
                        && cell.flags.contains(line.flag)
                        && cell.fg == start.fg
                        && cell.column == end.col + 1
                    {
                        if size_info.cols() == cell.column && size_info.lines() == cell.line {
                            // Add the last rect if we've reached the end of the terminal
                            self.inner.push(create_rect(
                                &start,
                                cell.into(),
                                line.flag,
                                &self.metrics,
                                &self.size,
                                offset,
                            ));
                        } else {
                            // Update the length of the line
                            *end = cell.into();
                        }

                        continue;
                    }

                    self.inner.push(create_rect(start, *end, line.flag, &self.metrics, &self.size, offset));

                    // Start a new line if the flag is present
                    if cell.flags.contains(line.flag) {
                        *start = cell.clone();
                        *end = cell.into();
                    } else {
                        line.range = None;
                    }
                },
                // Check for new start of line
                None => {
                    if cell.flags.contains(line.flag) {
                        line.range = Some((cell.clone(), cell.into()));
                    }
                },
            };
        }
    }

    // Add a rectangle
    pub fn push(&mut self, rect: Rect<f32>, color: Rgb) {
        self.inner.push((rect, color));
    }
}

/// Create a rectangle that starts on the left of `start` and ends on the right
/// of `end`, based on the given flag and size metrics.
fn create_rect(
    start: &RenderableCell,
    end: Point,
    flag: Flags,
    metrics: &Metrics,
    size: &SizeInfo,
    offset: (f32, f32),
) -> (Rect<f32>, Rgb) {
    let start_x = start.column.0 as f32 * size.cell_width;
    let end_x = (end.col.0 + 1) as f32 * size.cell_width;
    let width = end_x - start_x;

    let (position, mut height) = match flag {
        Flags::UNDERLINE => (metrics.underline_position, metrics.underline_thickness),
        Flags::STRIKEOUT => (metrics.strikeout_position, metrics.strikeout_thickness),
        _ => unimplemented!("Invalid flag for cell line drawing specified"),
    };

    // Make sure lines are always visible
    height = height.max(1.);

    let cell_bottom = (start.line.0 as f32 + 1.) * size.cell_height;
    let baseline = cell_bottom + metrics.descent;

    let mut y = baseline - position - height / 2.;
    let max_y = cell_bottom - height;
    if y > max_y {
        y = max_y;
    }

    let rect =
        Rect::new(start_x + size.padding_x + offset.0,
                  y.round() + size.padding_y + offset.1,
                  width, height.round());

    (rect, start.fg)
}
