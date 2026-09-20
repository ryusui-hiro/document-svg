//! Bounded NetCDF classic (CDF-1) and 64-bit-offset (CDF-2) preview.
//!
//! The classic formats are self-describing big-endian files containing
//! dimensions, attributes, and homogeneous variables. This reader exposes
//! that structure as inert tables and never evaluates metadata or follows
//! references. NetCDF-4/HDF5 and CDF-5 are reported as unsupported.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::table::{TableAlign, TableData};

const MAX_NETCDF_BYTES: u64 = 512 * 1024 * 1024;
const MAX_NETCDF_DIMS: usize = 1024;
const MAX_NETCDF_VARS: usize = 1024;
const MAX_NETCDF_ATTRIBUTES: usize = 4096;
const MAX_NETCDF_NAME_BYTES: usize = 4096;
const MAX_NETCDF_VALUES_PER_VAR: usize = 200_000;
const MAX_NETCDF_TOTAL_VALUES: usize = 1_000_000;
const MAX_NETCDF_TEXT_BYTES: usize = 64 * 1024 * 1024;
const MAX_NETCDF_VALUE_CHARS: usize = 512;

pub(crate) fn looks_like_prefix(bytes: &[u8]) -> bool {
    bytes.len() >= 4 && bytes[..3] == *b"CDF" && matches!(bytes[3], 1 | 2 | 5)
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    convert_with_profile(path, options, sink, "netcdf", "NetCDF dataset", None)
}

pub(crate) fn convert_exodus(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    convert_with_profile(
        path,
        options,
        sink,
        "exodus",
        "Exodus II dataset",
        Some(
            "Exodus-specific mesh topology, element blocks, sets, result semantics, and solver operations are not reconstructed",
        ),
    )
}

fn convert_with_profile(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
    source_format: &'static str,
    title: &'static str,
    profile_warning: Option<&'static str>,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_NETCDF_BYTES),
        if source_format == "exodus" {
            "Exodus II input"
        } else {
            "NetCDF input"
        },
    )?;
    let (dataset, mut warnings) = parse_netcdf(bytes)?;
    if let Some(profile_warning) = profile_warning {
        warnings.push(profile_warning.into());
    }
    let mut blocks = Vec::new();
    blocks.push(HtmlBlock::Heading {
        level: 1,
        text: if source_format == "netcdf" {
            format!("NetCDF {} dataset", dataset.variant)
        } else {
            format!("{title} ({})", dataset.variant)
        },
    });
    if !dataset.dimensions.is_empty() {
        blocks.push(HtmlBlock::Heading {
            level: 2,
            text: "Dimensions".into(),
        });
        blocks.push(HtmlBlock::Table(TableData {
            headers: vec!["Name".into(), "Length".into(), "Role".into()],
            rows: dataset
                .dimensions
                .iter()
                .map(|dimension| {
                    vec![
                        dimension.name.clone(),
                        if dimension.unlimited {
                            dataset.num_records.to_string()
                        } else {
                            dimension.length.to_string()
                        },
                        if dimension.unlimited {
                            "unlimited record dimension".into()
                        } else {
                            "fixed".into()
                        },
                    ]
                })
                .collect(),
            alignments: vec![TableAlign::Left, TableAlign::Right, TableAlign::Left],
            raw_source: String::new(),
        }));
    }
    if !dataset.global_attributes.is_empty() {
        blocks.push(HtmlBlock::Heading {
            level: 2,
            text: "Global attributes".into(),
        });
        blocks.push(HtmlBlock::Table(TableData {
            headers: vec!["Name".into(), "Type".into(), "Value".into()],
            rows: dataset
                .global_attributes
                .iter()
                .map(|attribute| {
                    vec![
                        attribute.name.clone(),
                        attribute.kind.name().into(),
                        attribute.display_value(),
                    ]
                })
                .collect(),
            alignments: vec![TableAlign::Left; 3],
            raw_source: String::new(),
        }));
    }
    for variable in &dataset.variables {
        blocks.push(HtmlBlock::Heading {
            level: 2,
            text: format!(
                "{} ({}, {})",
                variable.name,
                variable.kind.name(),
                variable.dimension_label(&dataset.dimensions)
            ),
        });
        if !variable.attributes.is_empty() {
            blocks.push(HtmlBlock::Table(TableData {
                headers: vec!["Attribute".into(), "Type".into(), "Value".into()],
                rows: variable
                    .attributes
                    .iter()
                    .map(|attribute| {
                        vec![
                            attribute.name.clone(),
                            attribute.kind.name().into(),
                            attribute.display_value(),
                        ]
                    })
                    .collect(),
                alignments: vec![TableAlign::Left; 3],
                raw_source: String::new(),
            }));
        }
        let rows = dataset.read_variable_rows(variable)?;
        if rows.is_empty() {
            warnings.push(format!(
                "{source_format} variable '{}' has no values within the preview budget",
                variable.name
            ));
        } else {
            blocks.push(HtmlBlock::Table(TableData {
                headers: vec!["Index".into(), "Value".into()],
                rows,
                alignments: vec![TableAlign::Right, TableAlign::Left],
                raw_source: String::new(),
            }));
        }
    }
    let mut page_sink = NetcdfPageSink {
        inner: sink,
        source_format,
        title,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    warnings.sort();
    warnings.dedup();
    Ok(warnings)
}

struct NetcdfPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    source_format: &'static str,
    title: &'static str,
    warnings: &'a [String],
}

impl PageConsumer for NetcdfPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = self.source_format.into();
        page.title = self.title.into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

#[derive(Clone, Debug)]
struct Dimension {
    name: String,
    length: usize,
    unlimited: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NcType {
    Byte,
    Char,
    Short,
    Int,
    Float,
    Double,
}

impl NcType {
    fn from_tag(tag: u32) -> Result<Self> {
        match tag {
            1 => Ok(Self::Byte),
            2 => Ok(Self::Char),
            3 => Ok(Self::Short),
            4 => Ok(Self::Int),
            5 => Ok(Self::Float),
            6 => Ok(Self::Double),
            _ => Err(Error::Unsupported(format!(
                "NetCDF type tag {tag} is unsupported"
            ))),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Byte => "byte",
            Self::Char => "char",
            Self::Short => "short",
            Self::Int => "int",
            Self::Float => "float",
            Self::Double => "double",
        }
    }

    fn bytes(self) -> usize {
        match self {
            Self::Byte | Self::Char => 1,
            Self::Short => 2,
            Self::Int | Self::Float => 4,
            Self::Double => 8,
        }
    }
}

#[derive(Clone, Debug)]
struct Attribute {
    name: String,
    kind: NcType,
    values: Vec<u8>,
    count: usize,
}

impl Attribute {
    fn display_value(&self) -> String {
        if self.kind == NcType::Char {
            let text = String::from_utf8_lossy(&self.values)
                .trim_end_matches('\0')
                .to_string();
            return truncate(&text);
        }
        let mut values = Vec::new();
        for index in 0..self.count.min(16) {
            if let Some(value) = decode_value(self.kind, &self.values, index) {
                values.push(value);
            }
        }
        if self.count > 16 {
            values.push("…".into());
        }
        truncate(&values.join(", "))
    }
}

#[derive(Clone, Debug)]
struct Variable {
    name: String,
    dimensions: Vec<usize>,
    attributes: Vec<Attribute>,
    kind: NcType,
    vsize: u64,
    begin: u64,
}

impl Variable {
    fn record(&self, unlimited_dim: Option<usize>) -> bool {
        self.dimensions.first().copied() == unlimited_dim
    }

    fn element_count(&self, dimensions: &[Dimension], records: usize) -> Result<usize> {
        let mut count = 1usize;
        for (index, dimension_id) in self.dimensions.iter().enumerate() {
            let dimension = dimensions.get(*dimension_id).ok_or_else(|| {
                Error::InvalidInput(format!(
                    "NetCDF variable '{}' references unknown dimension {}",
                    self.name, dimension_id
                ))
            })?;
            let length = if index == 0 && dimension.unlimited {
                records
            } else {
                dimension.length
            };
            count = count
                .checked_mul(length)
                .ok_or_else(|| Error::LimitExceeded("NetCDF variable size overflowed".into()))?;
        }
        Ok(count)
    }

    fn dimension_label(&self, dimensions: &[Dimension]) -> String {
        if self.dimensions.is_empty() {
            return "scalar".into();
        }
        self.dimensions
            .iter()
            .filter_map(|id| dimensions.get(*id).map(|dimension| dimension.name.as_str()))
            .collect::<Vec<_>>()
            .join(" × ")
    }

    fn fill_value(&self) -> Option<String> {
        self.attributes
            .iter()
            .find(|attribute| attribute.name == "_FillValue" && attribute.count == 1)
            .and_then(|attribute| decode_value(attribute.kind, &attribute.values, 0))
    }
}

#[derive(Debug)]
struct Dataset {
    variant: &'static str,
    bytes: Vec<u8>,
    dimensions: Vec<Dimension>,
    global_attributes: Vec<Attribute>,
    variables: Vec<Variable>,
    num_records: usize,
    record_size: u64,
}

impl Dataset {
    fn read_variable_rows(&self, variable: &Variable) -> Result<Vec<Vec<String>>> {
        let unlimited = self
            .dimensions
            .iter()
            .position(|dimension| dimension.unlimited);
        let count = variable.element_count(&self.dimensions, self.num_records)?;
        let wanted = count.min(MAX_NETCDF_VALUES_PER_VAR);
        let mut rows = Vec::with_capacity(wanted);
        let fill = variable.fill_value();
        for index in 0..wanted {
            let offset = self.value_offset(variable, index, unlimited)?;
            let value = read_value(variable.kind, &self.bytes, offset)?;
            let display = if fill.as_deref() == Some(value.as_str()) {
                "_FillValue".into()
            } else {
                truncate(&value)
            };
            rows.push(vec![index.to_string(), display]);
        }
        Ok(rows)
    }

    fn value_offset(
        &self,
        variable: &Variable,
        index: usize,
        unlimited: Option<usize>,
    ) -> Result<usize> {
        if !variable.record(unlimited) {
            let byte_offset = variable
                .begin
                .checked_add((index as u64).saturating_mul(variable.kind.bytes() as u64))
                .ok_or_else(|| Error::LimitExceeded("NetCDF data offset overflowed".into()))?;
            return usize::try_from(byte_offset).map_err(|_| {
                Error::LimitExceeded("NetCDF data offset exceeds platform size".into())
            });
        }
        if unlimited.is_none() {
            return Err(Error::InvalidInput(
                "NetCDF record variable has no unlimited dimension".into(),
            ));
        }
        let inner_count = variable.element_count(&self.dimensions, 1)?.max(1);
        let record_index = index / inner_count;
        let inner_index = index % inner_count;
        if record_index >= self.num_records {
            return Err(Error::InvalidInput(
                "NetCDF record index is outside numrecs".into(),
            ));
        }
        let byte_offset = variable
            .begin
            .checked_add((record_index as u64).saturating_mul(self.record_size))
            .and_then(|value| {
                value.checked_add((inner_index as u64).saturating_mul(variable.kind.bytes() as u64))
            })
            .ok_or_else(|| Error::LimitExceeded("NetCDF record data offset overflowed".into()))?;
        usize::try_from(byte_offset)
            .map_err(|_| Error::LimitExceeded("NetCDF record offset exceeds platform size".into()))
    }
}

fn parse_netcdf(bytes: Vec<u8>) -> Result<(Dataset, Vec<String>)> {
    if bytes.len() < 4 || &bytes[..3] != b"CDF" {
        return Err(Error::InvalidInput("NetCDF CDF magic is missing".into()));
    }
    let version = bytes[3];
    if version == 5 {
        return Err(Error::Unsupported(
            "NetCDF CDF-5 uses a different 64-bit header model and is unsupported".into(),
        ));
    }
    if !matches!(version, 1 | 2) {
        return Err(Error::Unsupported(format!(
            "NetCDF CDF version {version} is unsupported; expected CDF-1 or CDF-2"
        )));
    }
    let mut reader = Reader::new(&bytes[4..], version == 2);
    let numrecs_raw = reader.u32("numrecs")?;
    let dimensions = reader.dimensions()?;
    let global_attributes = reader.attributes("global attributes")?;
    let variables = reader.variables()?;
    let attribute_bytes = global_attributes
        .iter()
        .chain(
            variables
                .iter()
                .flat_map(|variable| variable.attributes.iter()),
        )
        .map(|attribute| attribute.values.len())
        .try_fold(0usize, |sum, value| sum.checked_add(value))
        .ok_or_else(|| Error::LimitExceeded("NetCDF attribute byte size overflowed".into()))?;
    if attribute_bytes > MAX_NETCDF_TEXT_BYTES {
        return Err(Error::LimitExceeded(format!(
            "NetCDF attribute data exceeds {MAX_NETCDF_TEXT_BYTES} bytes"
        )));
    }
    if reader.position > bytes.len() {
        return Err(Error::InvalidInput(
            "NetCDF header extends beyond the file".into(),
        ));
    }
    let unlimited = dimensions.iter().position(|dimension| dimension.unlimited);
    if unlimited.is_none() && numrecs_raw != 0 && numrecs_raw != u32::MAX {
        return Err(Error::InvalidInput(
            "NetCDF has a nonzero record count but no unlimited dimension".into(),
        ));
    }
    let num_records = if numrecs_raw == u32::MAX {
        0
    } else {
        usize::try_from(numrecs_raw)
            .map_err(|_| Error::LimitExceeded("NetCDF numrecs exceeds platform size".into()))?
    };
    let record_variables = variables
        .iter()
        .filter(|variable| variable.record(unlimited))
        .collect::<Vec<_>>();
    let mut record_size = record_variables.iter().try_fold(0u64, |sum, variable| {
        sum.checked_add(variable.vsize)
            .ok_or_else(|| Error::LimitExceeded("NetCDF record size overflowed".into()))
    })?;
    if record_variables.len() == 1
        && matches!(
            record_variables[0].kind,
            NcType::Byte | NcType::Char | NcType::Short
        )
    {
        let raw_record_values = record_variables[0].element_count(&dimensions, 1)?;
        record_size =
            (raw_record_values as u64).saturating_mul(record_variables[0].kind.bytes() as u64);
    }
    let mut effective_records = num_records;
    let mut warnings = Vec::new();
    if numrecs_raw == u32::MAX {
        if record_size == 0 {
            effective_records = 0;
        } else {
            let starts = record_variables
                .iter()
                .map(|variable| variable.begin)
                .min()
                .unwrap_or(bytes.len() as u64);
            effective_records = ((bytes.len() as u64).saturating_sub(starts) / record_size)
                .try_into()
                .map_err(|_| {
                    Error::LimitExceeded("NetCDF streaming record count is too large".into())
                })?;
        }
        warnings.push(
            "NetCDF numrecs uses the streaming sentinel; record count was derived from file length"
                .into(),
        );
    }
    if effective_records > MAX_NETCDF_VALUES_PER_VAR {
        return Err(Error::LimitExceeded(format!(
            "NetCDF record count exceeds {MAX_NETCDF_VALUES_PER_VAR}"
        )));
    }
    let mut total_values = 0usize;
    for variable in &variables {
        for (index, dimension_id) in variable.dimensions.iter().enumerate() {
            if dimensions
                .get(*dimension_id)
                .is_some_and(|dimension| dimension.unlimited && index != 0)
            {
                return Err(Error::Unsupported(format!(
                    "NetCDF variable '{}' uses an unlimited dimension after its first dimension",
                    variable.name
                )));
            }
        }
        let count = variable.element_count(&dimensions, effective_records)?;
        total_values = total_values
            .checked_add(count.min(MAX_NETCDF_VALUES_PER_VAR))
            .ok_or_else(|| Error::LimitExceeded("NetCDF total value count overflowed".into()))?;
        if total_values > MAX_NETCDF_TOTAL_VALUES {
            return Err(Error::LimitExceeded(format!(
                "NetCDF values exceed {MAX_NETCDF_TOTAL_VALUES}"
            )));
        }
        let data_bytes = if variable.record(unlimited) {
            let per_record = if record_variables.len() == 1
                && matches!(variable.kind, NcType::Byte | NcType::Char | NcType::Short)
            {
                record_size
            } else {
                variable.vsize
            };
            per_record.saturating_mul(effective_records as u64)
        } else {
            variable.vsize
        };
        if variable
            .begin
            .checked_add(data_bytes)
            .is_none_or(|end| end > bytes.len() as u64)
        {
            return Err(Error::InvalidInput(format!(
                "NetCDF variable '{}' data extends beyond the file",
                variable.name
            )));
        }
    }
    let dataset = Dataset {
        variant: if version == 1 { "CDF-1" } else { "CDF-2" },
        bytes,
        dimensions,
        global_attributes,
        variables,
        num_records: effective_records,
        record_size,
    };
    Ok((dataset, warnings))
}

struct Reader<'a> {
    bytes: &'a [u8],
    position: usize,
    wide_offsets: bool,
    unlimited: Option<usize>,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8], wide_offsets: bool) -> Self {
        Self {
            bytes,
            position: 0,
            wide_offsets,
            unlimited: None,
        }
    }

    fn bytes(&mut self, count: usize, context: &str) -> Result<&'a [u8]> {
        let end = self
            .position
            .checked_add(count)
            .ok_or_else(|| Error::LimitExceeded(format!("NetCDF {context} offset overflowed")))?;
        let slice = self.bytes.get(self.position..end).ok_or_else(|| {
            Error::InvalidInput(format!(
                "NetCDF {context} ends before the header is complete"
            ))
        })?;
        self.position = end;
        Ok(slice)
    }

    fn u32(&mut self, context: &str) -> Result<u32> {
        let bytes = self.bytes(4, context)?;
        Ok(u32::from_be_bytes(bytes.try_into().unwrap()))
    }

    fn u64(&mut self, context: &str) -> Result<u64> {
        let bytes = self.bytes(8, context)?;
        Ok(u64::from_be_bytes(bytes.try_into().unwrap()))
    }

    fn offset(&mut self, context: &str) -> Result<u64> {
        if self.wide_offsets {
            self.u64(context)
        } else {
            Ok(u64::from(self.u32(context)?))
        }
    }

    fn padded_bytes(&mut self, count: usize, context: &str) -> Result<Vec<u8>> {
        let data = self.bytes(count, context)?.to_vec();
        let padded = (count + 3) & !3;
        if padded > count {
            let _ = self.bytes(padded - count, "padding")?;
        }
        Ok(data)
    }

    fn name(&mut self, context: &str) -> Result<String> {
        let length = usize::try_from(self.u32(context)?)
            .map_err(|_| Error::LimitExceeded("NetCDF name length exceeds platform size".into()))?;
        if length == 0 || length > MAX_NETCDF_NAME_BYTES {
            return Err(Error::LimitExceeded(format!(
                "NetCDF {context} name length {length} is outside the supported range"
            )));
        }
        let bytes = self.padded_bytes(length, context)?;
        if bytes.contains(&0) {
            return Err(Error::InvalidInput(format!(
                "NetCDF {context} name contains NUL"
            )));
        }
        String::from_utf8(bytes).map_err(|error| {
            Error::InvalidInput(format!("NetCDF {context} name is not UTF-8: {error}"))
        })
    }

    fn dimensions(&mut self) -> Result<Vec<Dimension>> {
        let tag = self.u32("dimension list tag")?;
        if tag == 0 {
            self.unlimited = None;
            return Ok(Vec::new());
        }
        if tag != 10 {
            return Err(Error::InvalidInput(format!(
                "NetCDF dimension list has unexpected tag {tag}"
            )));
        }
        let count = checked_count(self.u32("dimension count")?, MAX_NETCDF_DIMS, "dimensions")?;
        let mut dimensions = Vec::with_capacity(count);
        for index in 0..count {
            let name = self.name("dimension")?;
            let length = usize::try_from(self.u32("dimension length")?).map_err(|_| {
                Error::LimitExceeded("NetCDF dimension length exceeds platform size".into())
            })?;
            let unlimited = length == 0;
            if unlimited {
                if self.unlimited.is_some() {
                    return Err(Error::InvalidInput(
                        "NetCDF declares more than one unlimited dimension".into(),
                    ));
                }
                self.unlimited = Some(index);
            }
            dimensions.push(Dimension {
                name,
                length,
                unlimited,
            });
        }
        Ok(dimensions)
    }

    fn attributes(&mut self, context: &str) -> Result<Vec<Attribute>> {
        let tag = self.u32(&format!("{context} tag"))?;
        if tag == 0 {
            return Ok(Vec::new());
        }
        if tag != 12 {
            return Err(Error::InvalidInput(format!(
                "NetCDF {context} has unexpected tag {tag}"
            )));
        }
        let count = checked_count(
            self.u32(&format!("{context} count"))?,
            MAX_NETCDF_ATTRIBUTES,
            context,
        )?;
        let mut attributes = Vec::with_capacity(count);
        for _ in 0..count {
            let name = self.name("attribute")?;
            let kind = NcType::from_tag(self.u32("attribute type")?)?;
            let count = checked_count(
                self.u32("attribute value count")?,
                MAX_NETCDF_VALUES_PER_VAR,
                "attribute values",
            )?;
            let bytes = count.checked_mul(kind.bytes()).ok_or_else(|| {
                Error::LimitExceeded("NetCDF attribute byte size overflowed".into())
            })?;
            if bytes > MAX_NETCDF_TEXT_BYTES {
                return Err(Error::LimitExceeded(
                    "NetCDF attribute values exceed the text budget".into(),
                ));
            }
            let values = self.padded_bytes(bytes, "attribute values")?;
            attributes.push(Attribute {
                name,
                kind,
                values,
                count,
            });
        }
        Ok(attributes)
    }

    fn variables(&mut self) -> Result<Vec<Variable>> {
        let tag = self.u32("variable list tag")?;
        if tag == 0 {
            return Ok(Vec::new());
        }
        if tag != 11 {
            return Err(Error::InvalidInput(format!(
                "NetCDF variable list has unexpected tag {tag}"
            )));
        }
        let count = checked_count(self.u32("variable count")?, MAX_NETCDF_VARS, "variables")?;
        let mut variables = Vec::with_capacity(count);
        for _ in 0..count {
            let name = self.name("variable")?;
            let rank = checked_count(
                self.u32("variable rank")?,
                MAX_NETCDF_DIMS,
                "variable dimensions",
            )?;
            let mut dimensions = Vec::with_capacity(rank);
            for _ in 0..rank {
                let dimension_id = checked_index(
                    self.u32("variable dimension id")?,
                    MAX_NETCDF_DIMS,
                    "variable dimension",
                )?;
                dimensions.push(dimension_id);
            }
            let attributes = self.attributes("variable attributes")?;
            let kind = NcType::from_tag(self.u32("variable type")?)?;
            let vsize = u64::from(self.u32("variable vsize")?);
            let begin = self.offset("variable begin offset")?;
            variables.push(Variable {
                name,
                dimensions,
                attributes,
                kind,
                vsize,
                begin,
            });
        }
        Ok(variables)
    }
}

fn checked_count(raw: u32, maximum: usize, context: &str) -> Result<usize> {
    let count = usize::try_from(raw).map_err(|_| {
        Error::LimitExceeded(format!("NetCDF {context} count exceeds platform size"))
    })?;
    if count > maximum {
        return Err(Error::LimitExceeded(format!(
            "NetCDF {context} count {count} exceeds {maximum}"
        )));
    }
    Ok(count)
}

fn checked_index(raw: u32, maximum: usize, context: &str) -> Result<usize> {
    let index = usize::try_from(raw).map_err(|_| {
        Error::LimitExceeded(format!("NetCDF {context} index exceeds platform size"))
    })?;
    if index >= maximum {
        return Err(Error::InvalidInput(format!(
            "NetCDF {context} index {index} is out of range"
        )));
    }
    Ok(index)
}

fn decode_value(kind: NcType, bytes: &[u8], index: usize) -> Option<String> {
    let width = kind.bytes();
    let start = index.checked_mul(width)?;
    let value = bytes.get(start..start + width)?;
    Some(match kind {
        NcType::Byte => (value[0] as i8).to_string(),
        NcType::Char => String::from_utf8_lossy(value).to_string(),
        NcType::Short => i16::from_be_bytes(value.try_into().ok()?).to_string(),
        NcType::Int => i32::from_be_bytes(value.try_into().ok()?).to_string(),
        NcType::Float => f32::from_bits(u32::from_be_bytes(value.try_into().ok()?)).to_string(),
        NcType::Double => f64::from_bits(u64::from_be_bytes(value.try_into().ok()?)).to_string(),
    })
}

fn read_value(kind: NcType, bytes: &[u8], offset: usize) -> Result<String> {
    let width = kind.bytes();
    let value = bytes
        .get(offset..offset.saturating_add(width))
        .ok_or_else(|| {
            Error::InvalidInput("NetCDF variable data ends before a complete value".into())
        })?;
    decode_value(kind, value, 0)
        .ok_or_else(|| Error::InvalidInput("NetCDF variable value could not be decoded".into()))
}

fn truncate(value: &str) -> String {
    value.chars().take(MAX_NETCDF_VALUE_CHARS).collect()
}

#[cfg(test)]
mod tests {
    use super::{looks_like_prefix, parse_netcdf};

    fn tiny_cdf1() -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"CDF\x01");
        bytes.extend_from_slice(&0u32.to_be_bytes()); // numrecs
        bytes.extend_from_slice(&10u32.to_be_bytes()); // dimensions tag
        bytes.extend_from_slice(&1u32.to_be_bytes());
        bytes.extend_from_slice(&1u32.to_be_bytes());
        bytes.extend_from_slice(b"x\0\0\0");
        bytes.extend_from_slice(&3u32.to_be_bytes());
        bytes.extend_from_slice(&0u32.to_be_bytes()); // global attrs absent
        bytes.extend_from_slice(&11u32.to_be_bytes()); // vars tag
        bytes.extend_from_slice(&1u32.to_be_bytes());
        bytes.extend_from_slice(&1u32.to_be_bytes());
        bytes.extend_from_slice(b"v\0\0\0");
        bytes.extend_from_slice(&1u32.to_be_bytes()); // rank
        bytes.extend_from_slice(&0u32.to_be_bytes()); // dim id
        bytes.extend_from_slice(&0u32.to_be_bytes()); // attrs absent
        bytes.extend_from_slice(&4u32.to_be_bytes()); // int
        bytes.extend_from_slice(&12u32.to_be_bytes()); // vsize
        bytes.extend_from_slice(&72u32.to_be_bytes()); // begin
        for value in [1i32, 2, 3] {
            bytes.extend_from_slice(&value.to_be_bytes());
        }
        bytes
    }

    fn single_char_record_cdf1() -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"CDF\x01");
        bytes.extend_from_slice(&2u32.to_be_bytes());
        bytes.extend_from_slice(&10u32.to_be_bytes());
        bytes.extend_from_slice(&1u32.to_be_bytes());
        bytes.extend_from_slice(&4u32.to_be_bytes());
        bytes.extend_from_slice(b"time");
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes.extend_from_slice(&11u32.to_be_bytes());
        bytes.extend_from_slice(&1u32.to_be_bytes());
        bytes.extend_from_slice(&1u32.to_be_bytes());
        bytes.extend_from_slice(b"c\0\0\0");
        bytes.extend_from_slice(&1u32.to_be_bytes());
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes.extend_from_slice(&2u32.to_be_bytes());
        bytes.extend_from_slice(&4u32.to_be_bytes());
        let begin = bytes.len() + 4;
        bytes.extend_from_slice(&(begin as u32).to_be_bytes());
        bytes.extend_from_slice(b"ab");
        bytes
    }

    #[test]
    fn parses_big_endian_classic_dimensions_and_values() {
        let bytes = tiny_cdf1();
        let (dataset, warnings) = parse_netcdf(bytes).unwrap();
        assert!(warnings.is_empty());
        assert_eq!(dataset.variant, "CDF-1");
        assert_eq!(dataset.variables[0].name, "v");
        assert_eq!(
            dataset.read_variable_rows(&dataset.variables[0]).unwrap()[1][1],
            "2"
        );
    }

    #[test]
    fn recognizes_only_supported_cdf_magic_versions() {
        assert!(looks_like_prefix(b"CDF\x01"));
        assert!(looks_like_prefix(b"CDF\x02"));
        assert!(looks_like_prefix(b"CDF\x05"));
        assert!(!looks_like_prefix(b"HDF\x89"));
    }

    #[test]
    fn handles_the_unpadded_single_byte_record_special_case() {
        let bytes = single_char_record_cdf1();
        let (dataset, _) = parse_netcdf(bytes).unwrap();
        let rows = dataset.read_variable_rows(&dataset.variables[0]).unwrap();
        assert_eq!(rows[0][1], "a");
        assert_eq!(rows[1][1], "b");
    }

    #[test]
    fn rejects_unexpected_header_list_tags() {
        let mut bytes = tiny_cdf1();
        bytes[8..12].copy_from_slice(&99u32.to_be_bytes());
        let error = parse_netcdf(bytes).unwrap_err();
        assert!(error.to_string().contains("unexpected tag"));
    }
}
