use super::cargo_cmds::{cargo_fmt, cargo_fmt_file, cargo_run, MAIN_FILE};
use super::highlight::highlight;
use crate::irust::format::{format_err, format_eval_output, output_is_err};
use crate::irust::printer::{Printer, PrinterItem, PrinterItemType};
use crate::irust::{IRust, IRustError};
use crate::utils::{remove_main, stdout_and_stderr};

const SUCCESS: &str = "Ok!";

impl IRust {
    pub fn parse(&mut self) -> Result<Printer, IRustError> {
        match self.buffer.to_string().as_str() {
            ":help" => self.help(),
            ":reset" => self.reset(),
            ":show" => self.show(),
            ":pop" => self.pop(),
            ":irust" => self.irust(),
            cmd if cmd.starts_with("::") => self.run_cmd(),
            cmd if cmd.starts_with(":edit") => self.extern_edit(),
            cmd if cmd.starts_with(":add") => self.add_dep(),
            cmd if cmd.starts_with(":load") => self.load(),
            cmd if cmd.starts_with(":reload") => self.reload(),
            cmd if cmd.starts_with(":type") => self.show_type(),
            cmd if cmd.starts_with(":del") => self.del(),
            cmd if cmd.starts_with(":cd") => self.cd(),
            _ => self.parse_second_order(),
        }
    }

    fn reset(&mut self) -> Result<Printer, IRustError> {
        self.repl.reset();
        let mut outputs = Printer::new(PrinterItem::new(SUCCESS.to_string(), PrinterItemType::Ok));
        outputs.add_new_line(1);

        Ok(outputs)
    }

    fn pop(&mut self) -> Result<Printer, IRustError> {
        self.repl.pop();
        let mut outputs = Printer::new(PrinterItem::new(SUCCESS.to_string(), PrinterItemType::Ok));
        outputs.add_new_line(1);

        Ok(outputs)
    }

    fn del(&mut self) -> Result<Printer, IRustError> {
        if let Some(line_num) = self.buffer.to_string().split_whitespace().last() {
            self.repl.del(line_num)?;
        }

        let mut outputs = Printer::new(PrinterItem::new(SUCCESS.to_string(), PrinterItemType::Ok));
        outputs.add_new_line(1);

        Ok(outputs)
    }

    fn show(&mut self) -> Result<Printer, IRustError> {
        let repl_code = highlight(self.repl.show());

        Ok(repl_code)
    }

    fn add_dep(&mut self) -> Result<Printer, IRustError> {
        let mut dep: Vec<String> = self
            .buffer
            .to_string()
            .split_whitespace()
            .skip(1)
            .map(ToOwned::to_owned)
            .collect();

        // if user provides a path, for example for cargo add --path
        // canonicalize that path, since relative directiories wont work
        for p in dep.iter_mut() {
            if p.contains('/') {
                match std::path::Path::new(p).canonicalize() {
                    Ok(full_p) => {
                        if let Some(full_p) = full_p.to_str() {
                            *p = full_p.to_string();
                        }
                    }
                    Err(e) => return Err(e.into()),
                }
            }
        }

        self.cursor.save_position()?;
        self.wait_add(self.repl.add_dep(&dep)?, "Add")?;
        self.wait_add(self.repl.build()?, "Build")?;
        self.write_newline()?;

        let mut outputs = Printer::new(PrinterItem::new(SUCCESS.to_string(), PrinterItemType::Ok));
        outputs.add_new_line(1);

        Ok(outputs)
    }

    fn load(&mut self) -> Result<Printer, IRustError> {
        let buffer = self.buffer.to_string();
        let path = if let Some(path) = buffer.split_whitespace().nth(1) {
            std::path::Path::new(&path).to_path_buf()
        } else {
            return Err("No path specified").map_err(|e| e.into());
        };
        self.load_inner(path)
    }

    fn reload(&mut self) -> Result<Printer, IRustError> {
        let path = if let Some(path) = self.known_paths.get_last_loaded_coded_path() {
            path
        } else {
            return Err("No saved path").map_err(|e| e.into());
        };
        self.load_inner(path)
    }

    pub fn load_inner(&mut self, path: std::path::PathBuf) -> Result<Printer, IRustError> {
        // save path
        self.known_paths.set_last_loaded_coded_path(path.clone());

        // reset repl
        self.repl.reset();

        // read code
        let path_code = std::fs::read(path)?;
        let code = if let Ok(code) = String::from_utf8(path_code) {
            code
        } else {
            return Err("The specified file is not utf8 encoded").map_err(Into::into);
        };

        // Format code to make `remove_main` function work correctly
        let code = cargo_fmt(&code)?;
        let code = remove_main(&code);

        // build the code
        let output = self.repl.eval_build(code.clone())?;

        if output_is_err(&output) {
            Ok(format_err(&output))
        } else {
            self.repl.insert(code);
            let mut outputs =
                Printer::new(PrinterItem::new(SUCCESS.to_string(), PrinterItemType::Ok));
            outputs.add_new_line(1);
            Ok(outputs)
        }
    }

    fn show_type(&mut self) -> Result<Printer, IRustError> {
        // TODO
        // We should probably use the `Any` trait instead of the current method
        // Current method might break with compiler updates
        // On the other hand `Any` is more limited

        const TYPE_FOUND_MSG: &str = "expected `()`, found ";
        const EMPTY_TYPE_MSG: &str = "dev [unoptimized + debuginfo]";

        let variable = self
            .buffer
            .to_string()
            .trim_start_matches(":type")
            .to_string();
        let mut raw_out = String::new();

        self.repl
            .eval_in_tmp_repl(variable, || -> Result<(), IRustError> {
                raw_out = cargo_run(false).unwrap();
                Ok(())
            })?;

        let var_type = if raw_out.find(TYPE_FOUND_MSG).is_some() {
            raw_out
                .lines()
                // there is a case where there could be 2 found msg
                // the second one is more detailed
                .rev()
                .find(|l| l.contains("found"))
                .unwrap()
                .rsplit("found ")
                .next()
                .unwrap()
                .to_string()
        } else if raw_out.find(EMPTY_TYPE_MSG).is_some() {
            "()".into()
        } else {
            "Uknown".into()
        };

        Ok(Printer::new(PrinterItem::new(
            var_type,
            PrinterItemType::Ok,
        )))
    }

    fn run_cmd(&mut self) -> Result<Printer, IRustError> {
        // remove ::
        let buffer = &self.buffer.to_string()[2..];

        let mut cmd = buffer.split_whitespace();
        let output = stdout_and_stderr(
            std::process::Command::new(cmd.next().unwrap_or_default())
                .args(&cmd.collect::<Vec<&str>>())
                .output()?,
        );

        Ok(Printer::new(PrinterItem::new(
            output,
            PrinterItemType::Shell,
        )))
    }

    fn parse_second_order(&mut self) -> Result<Printer, IRustError> {
        const FUNCTION_DEF: &str = "fn ";
        const ASYNC_FUNCTION_DEF: &str = "async fn ";
        const ENUM_DEF: &str = "enum ";
        const STRUCT_DEF: &str = "struct ";
        const TRAIT_DEF: &str = "trait ";
        const IMPL: &str = "impl ";
        const PUB: &str = "pub ";

        // attribute exp:
        // #[derive(Debug)]
        // struct B{}
        const ATTRIBUTE: &str = "#";

        let buffer = self.buffer.to_string();
        let buffer = buffer.trim();

        if buffer.is_empty() {
            Ok(Printer::default())
        } else if buffer.ends_with(';')
            || buffer.starts_with(FUNCTION_DEF)
            || buffer.starts_with(ASYNC_FUNCTION_DEF)
            || buffer.starts_with(ENUM_DEF)
            || buffer.starts_with(STRUCT_DEF)
            || buffer.starts_with(TRAIT_DEF)
            || buffer.starts_with(IMPL)
            || buffer.starts_with(ATTRIBUTE)
            || buffer.starts_with(PUB)
        {
            self.repl.insert(self.buffer.to_string());

            let printer = Printer::default();

            Ok(printer)
        } else {
            let mut outputs = Printer::default();
            if let Some(mut eval_output) =
                format_eval_output(&self.repl.eval(self.buffer.to_string())?)
            {
                outputs.append(&mut eval_output);
                outputs.add_new_line(1);
            }

            Ok(outputs)
        }
    }

    fn extern_edit(&mut self) -> Result<Printer, IRustError> {
        // exp: :edit vi
        let editor: String = match self.buffer.to_string().split_whitespace().nth(1) {
            Some(ed) => ed.to_string(),
            None => return Err(IRustError::Custom("No editor specified".to_string())),
        };

        self.raw_terminal.write_with_color(
            format!("waiting for {}...", editor),
            crossterm::style::Color::Magenta,
        )?;
        self.write_newline()?;

        // write current repl (to ensure eval leftover is cleaned)
        self.repl.write()?;
        // beautify code
        if self.repl.body.len() > 2 {
            let _ = cargo_fmt_file(&*MAIN_FILE);
        }

        std::process::Command::new(editor)
            .arg(&*MAIN_FILE)
            .spawn()?
            .wait()?;

        match self.repl.update_from_main_file() {
            Ok(_) => Ok(Printer::new(PrinterItem::new(
                SUCCESS.to_string(),
                PrinterItemType::Ok,
            ))),
            Err(e) => {
                self.repl.reset();
                Err(e)
            }
        }
    }

    fn irust(&mut self) -> Result<Printer, IRustError> {
        let irust = self.ferris();

        Ok(Printer::new(PrinterItem::new(
            irust,
            PrinterItemType::Custom(crossterm::style::Color::Red),
        )))
    }

    fn cd(&mut self) -> Result<Printer, IRustError> {
        use std::env::*;
        let buffer = self.buffer.to_string();
        let buffer = buffer
            .split(":cd")
            .skip(1)
            .collect::<String>()
            .trim()
            .to_string();
        match buffer.as_str() {
            "" => {
                if let Some(dir) = dirs_next::home_dir() {
                    set_current_dir(dir)?;
                }
            }
            "-" => {
                set_current_dir(self.known_paths.get_pwd())?;
            }
            path => {
                let mut dir = current_dir()?;
                dir.push(&path);
                set_current_dir(dir)?;
            }
        }
        // Update cwd and the terminal title accordingly
        let cwd = current_dir()?;
        self.known_paths.update_cwd(cwd.clone());
        self.raw_terminal
            .set_title(&format!("IRust: {}", cwd.display()));

        let mut output = Printer::new(PrinterItem::new(
            cwd.display().to_string(),
            PrinterItemType::Ok,
        ));
        output.add_new_line(1);
        Ok(output)
    }
}
