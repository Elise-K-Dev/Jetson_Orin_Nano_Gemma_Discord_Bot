use serenity::all::{
    CommandDataOptionValue, CommandOptionType, CreateCommand, CreateCommandOption,
};

pub const ASK: &str = "ask";
pub const ASK_PROMPT_OPTION: &str = "prompt";
pub const DEV: &str = "dev";
pub const DEV_TASK_OPTION: &str = "task";
pub const LLM_STATUS: &str = "llm-status";
pub const PING: &str = "ping";

pub fn all_commands(dev_sandbox_enabled: bool) -> Vec<CreateCommand> {
    let mut commands = vec![ping(), llm_status(), ask()];
    if dev_sandbox_enabled {
        commands.push(dev());
    }
    commands
}

pub fn dev() -> CreateCommand {
    CreateCommand::new(DEV)
        .description("격리된 개발 컨테이너에서 코미에게 작업을 맡깁니다.")
        .add_option(
            CreateCommandOption::new(
                CommandOptionType::String,
                DEV_TASK_OPTION,
                "컨테이너 안에서 수행할 개발 작업",
            )
            .required(true)
            .max_length(1800),
        )
}

pub fn ping() -> CreateCommand {
    CreateCommand::new(PING).description("봇 응답 상태를 확인합니다.")
}

pub fn llm_status() -> CreateCommand {
    CreateCommand::new(LLM_STATUS).description("Jetson의 로컬 llama-server 상태를 확인합니다.")
}

pub fn ask() -> CreateCommand {
    CreateCommand::new(ASK)
        .description("로컬 Gemma 모델에 질문합니다.")
        .add_option(
            CreateCommandOption::new(
                CommandOptionType::String,
                ASK_PROMPT_OPTION,
                "모델에 전달할 질문",
            )
            .required(true)
            .max_length(1800),
        )
}

pub fn string_option<'a>(
    options: &'a [serenity::all::CommandDataOption],
    name: &str,
) -> Option<&'a str> {
    options
        .iter()
        .find(|option| option.name == name)
        .and_then(|option| match &option.value {
            CommandDataOptionValue::String(value) => Some(value.as_str()),
            _ => None,
        })
}
