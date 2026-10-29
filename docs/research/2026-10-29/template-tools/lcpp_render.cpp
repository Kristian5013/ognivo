// Offline renderer: runs a chat template through llama.cpp's own pipeline
// (common_chat_templates_init + common_chat_templates_apply), the same code
// path llama-server uses for /v1/chat/completions. No model, no GPU.
// usage: lcpp_render TEMPLATE.jinja REQUEST.json
//   REQUEST.json: {messages, tools?, add_generation_prompt?, enable_thinking?,
//                  parallel_tool_calls?, chat_template_kwargs?{k: json}, bos_token?, eos_token?}
// stdout: rendered prompt; stderr: caps + handler outputs as JSON
#include "chat.h"

#include <cstdio>
#include <fstream>
#include <iostream>
#include <sstream>

static std::string slurp(const char * p) {
    std::ifstream f(p, std::ios::binary);
    if (!f) { throw std::runtime_error(std::string("cannot open ") + p); }
    std::ostringstream ss; ss << f.rdbuf(); return ss.str();
}

int main(int argc, char ** argv) {
    if (argc < 3) { fprintf(stderr, "usage: %s TEMPLATE REQUEST\n", argv[0]); return 2; }
    try {
        const std::string src = slurp(argv[1]);
        common_json req = common_json::parse(slurp(argv[2]));
        const std::string bos = req.value("bos_token", std::string(""));
        const std::string eos = req.value("eos_token", std::string(""));
        auto tmpls = common_chat_templates_init(nullptr, src, bos, eos);
        auto caps = common_chat_templates_get_caps(tmpls.get());

        common_chat_templates_inputs in;
        in.messages = common_chat_msgs_parse_oaicompat(req.at("messages"));
        if (req.contains("tools")) { in.tools = common_chat_tools_parse_oaicompat(req.at("tools")); }
        in.add_generation_prompt = req.value("add_generation_prompt", true);
        in.enable_thinking       = req.value("enable_thinking", true);
        in.parallel_tool_calls   = req.value("parallel_tool_calls", (bool) caps["supports_parallel_tool_calls"]);
        in.reasoning_format      = COMMON_REASONING_FORMAT_DEEPSEEK;
        in.use_jinja             = true;
        if (req.contains("chat_template_kwargs")) {
            for (const auto & it : req.at("chat_template_kwargs").items()) {
                in.chat_template_kwargs[it.key()] = it.value().dump();
            }
        }
        auto params = common_chat_templates_apply(tmpls.get(), in);
        std::cout << params.prompt.str();

        common_json meta = common_json::object();
        common_json jc = common_json::object();
        for (const auto & [k, v] : caps) { jc[k] = v; }
        meta["caps"] = jc;
        meta["format"] = std::string(common_chat_format_name(params.format));
        meta["generation_prompt"] = params.generation_prompt;
        meta["supports_thinking"] = params.supports_thinking;
        meta["thinking_start_tag"] = params.thinking_start_tag;
        common_json ends = common_json::array();
        for (const auto & e : params.thinking_end_tags) { ends.push_back(e); }
        meta["thinking_end_tags"] = ends;
        common_json pres = common_json::array();
        for (const auto & e : params.preserved_tokens) { pres.push_back(e); }
        meta["preserved_tokens"] = pres;
        common_json stops = common_json::array();
        for (const auto & e : params.additional_stops) { stops.push_back(e); }
        meta["additional_stops"] = stops;
        meta["grammar_lazy"] = params.grammar_lazy;
        meta["has_grammar"] = !params.grammar.empty();
        common_json trig = common_json::array();
        for (const auto & t : params.grammar_triggers) { trig.push_back(t.value); }
        meta["grammar_triggers"] = trig;
        int n_input_parts = 0;
        for (const auto & p : params.prompt.parts) { if (p.is_input) { n_input_parts++; } }
        meta["n_prompt_parts"] = (int) params.prompt.parts.size();
        meta["n_input_parts"] = n_input_parts;
        std::cerr << meta.dump(1) << std::endl;
    } catch (const std::exception & e) {
        std::cerr << "{\"error\": " << common_json(std::string(e.what())).dump() << "}" << std::endl;
        return 1;
    }
    return 0;
}
