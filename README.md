# Awful Text News

```sh
⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀
⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀
⠀⠀⢸⣿⣿⣶⣶⣦⣤⣀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⣀⣤⣴⣶⣶⠿⣿⡇⠀⠀
⠀⠀⢸⣿⣈⣉⠙⠛⠻⠿⣿⣶⣤⡀⠀⠀⢀⣤⣶⠿⠛⠋⠉⠀⠀⠀⢸⡇⠀⠀
⠀⠀⢸⣿⠛⠻⣿⢷⣶⣦⣤⣈⡉⣿⡇⢸⡟⠉⠀⠀⠀⠀⠀⠀⠀⢀⣼⡇⠀⠀
⠀⠀⢸⣿⠿⠶⣿⣤⣴⣿⣏⣉⣙⣿⡇⢸⡇⠀⠀⣀⣀⣤⣴⣶⠿⠿⣿⡇⠀⠀
⠀⠀⢸⣷⣶⣤⣤⣤⣄⣈⣉⠙⠛⣿⡇⢸⣷⠾⠟⠛⢉⣿⣧⣤⣴⣶⣿⡇⠀⠀
⠀⠀⢸⣯⣤⣄⣸⣿⣏⠙⠛⠛⠛⣿⡇⢸⣿⣴⣶⡿⠿⠛⠛⣿⣇⣤⣽⡇⠀⠀
⠀⠀⢸⡏⠉⠛⠛⠛⢿⡿⠿⢿⣶⣿⡇⢸⣿⣉⣤⣤⡶⠾⠛⠛⢉⣉⣽⡇⠀⠀
⠀⠀⢸⡇⠀⠀⠀⠀⢸⡷⠶⢤⣤⣿⡇⢸⣿⣉⣥⣿⣶⣶⠞⠛⠋⠉⢹⡇⠀⠀
⠀⠀⢸⡇⠀⠀⠀⠀⢸⣷⣦⣤⣤⣿⡇⢸⣿⠋⣉⣉⣨⣿⠀⣿⣿⡇⢸⡇⠀⠀
⠀⠀⢸⣇⣀⣀⣀⡀⢸⣧⣄⣉⣉⣿⡇⢸⣿⠛⠋⣉⣹⣿⣀⣉⣉⣠⣼⡇⠀⠀
⠀⠀⠈⠉⠉⠉⠉⠛⠛⠛⠿⠿⢿⣿⡇⢸⣿⡿⠿⠿⠛⠛⠛⠉⠉⠉⠉⠁⠀⠀
⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀
⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀
```

Awful Text News uses the [aj](https://github.com/graves/awful_aj) OpenAI-compatible API client library to summarize newspaper articles from sources that publish text-only or _lite_ versions of their stories. `awful_text_news` will then write the files to an [mdBook](https://github.com/rust-lang/mdBook) project, updating all necessary files to add a new _Edition_, which is what we call the output of a single execution.

**aj** supports _"Tool use"_ which allows us to specify a JSON Schema that the LLM will conform to. This is specified in a template file. An example file is available [here](./news_parser.yaml). You can specify the conversation's options in a configuration file. It must be `yaml` and an example is available [here](./config.yaml).

You can see a current working implementation at [news.awfulsec.com](https://news.awfulsec.com).

An API example is available at [news.awfulsec.com/api/2025-05-08/morning.json](https://news.awfulsec.com/api/2025-05-08/morning.json).

[news.awfulsec.com/api](https://news.awfulsec.com/api) provides a path for each day the project was executed with the edition name as the `json` file.

## Installation

[Install Rust.](https://www.rust-lang.org/tools/install)

[Install Conda.](https://docs.conda.io/projects/conda/en/latest/user-guide/install/index.html#regular-installation)

Install Python 3.11.0 and Pytorch.

```sh
conda install -c conda-forge python=3.11.0
conda install pytorch==2.4.0 torchvision==0.19.0 torchaudio==2.4.0 -c pytorch
```

Add the Pytorch libraries to your load path and instruct `sys-torch` to use Pytorch with an env variable.

```sh
export LIBTORCH=$HOME/miniconda3/lib/python3.11/site-packages/torch
export LD_LIBRARY_PATH=${LIBTORCH}/lib:$LD_LIBRARY_PATH
export LIBTORCH_USE_PYTORCH=1
```

*Note: LD_LIBRARY_PATH is DYLD_LIBRARY_PATH on MacOS*

```sh
cargo install awful_text_news
```

## Configuration

The important configuration options in `config.yaml` to adjust are:
- `api_key`: Your OpenAI-compatible API's key if it is protected by one.
- `api_base`: The URL where your OpenAI-compatible API is located. _Default setups require the `/v1` at the end.
- `model`: This specifies the LLM to use. This project was built using [Qwen_Qwen2.5-3B-Instruct-GGUF](https://huggingface.co/Qwen/Qwen2.5-3B-Instruct). The JSON Schema was adjusted until the model was able to output a mostly consistent representation of the data I was looking for. Experimentation with other models is highly recommended. Please let me know your experience if you do.

The rest of the configuration options can be safely ignored as they are specific to `aj`'s vector store support that allows details of the conversation to be recalled once they are no longer in the context window. This project relies on one-shot instructions.

The `news_parser.yaml` is a template file that specifies the system prompt, along with counterfeit user and assistant messages to guide the LLM into a style of correspondence or restrict output to a format. I found including at least one example of what I actually expect the output to be, greatly improves the results.

The application expects `config.yaml` to be in `com.awful-sec.aj` in your platform's system configuration directory on MacOS, or `$XDG_DIR` in linux, and `news_parser.yaml` to be in a subdirectory named `templates`.

```sh
λ tree `/Users/tg/Library/Application Support/com.awful-sec.aj`
/Users/tg/Library/Application Support/com.awful-sec.aj
├── config.yaml
└── templates
    ├── news_parser.yaml
```

## Use

### Run

```sh
awful_text_news  --json-output-dir . --markdown-output-dir /Users/tg/Projects/awful_security_news/src
```

### Expected output

```sh
Indexed 100 article urls from https://lite.cnn.com
Indexed 20 article urls from https://text.npr.org
Fetched 100 article contents from CNN
Fetched 20 article contents from NPR
Wrote JSON API file to ./2025-05-08/evening.json
Processed 1/120 articles
Processed 2/120 articles
Processed 3/120 articles
Processed 4/120 articles
...
...
Processed 117/120 articles
Processed 118/120 articles
Processed 119/120 articles
Processed 120/120 articles
Wrote FrontPage to awful_security_news/src/2025-05-08_evening.md
Updated TOC file at awful_security_news/src/2025-05-08.md
Updated SUMMARY.md
Updated daily_news.md
Execution time: 1566.37s (1566.366 seconds)
```

This will fetch the news articles from [https://lite.cnn.com](https://lite.cnn.com) and [https://text.npr.org](https://text.npr.org), then send their contents to be summarized. The model will also extract **named entities**, **key takeaways**, **important dates**, and **important timeframes**.

`awful_text_news` will write a `json` file to the location specified by the `--json-output-dir` flag. This file will be written to a subdirectory named after the date of execution and the `json` file itself will be named after the time of day.

```sh
λ cat ./2025-05-06/evening.json | jq | head -n 10
{
  "local_date": "2025-05-06",
  "time_of_day": "evening",
  "local_time": "02:08:07.627114",
  "articles": [
    {
      "source": "https://lite.cnn.com/2025/05/06/asia/us-philippines-exercise-target-ship-sinks-intl-hnk-ml",
      "dateOfPublication": "2025-05-06",
      "timeOfPublication": "02:03:00.000Z",
      "title": "A former US World War II-era warship sank before US and Philippine forces could use in drills",
```

This file is overwritten with an additional article every time one is processed. This allows us to use the file as a real-time API.

`awful_text_news` will also write a `markdown` representation to the location specified by the `--markdown-output-dir` flag. This file will be named `todays-date_time_of_day.md`. It allows the content to be read without distraction.


```sh
λ mdcat /Users/tg/Projects/awful_security_news/src/2025-05-06_evening.md | head -n 20
┄Awful Times

┄┄┄┄Edition published at 02:08:07.627114

┄┄A former US World War II-era warship sank before US and Philippine forces could use in drills

• source
• Published: 2025-05-06 02:03:00.000Z

┄┄┄Summary

A 1944 World War II-era US ship, the ex-USS Brattleboro, was scheduled to be used as the main target in the US-Philippine joint military drills, but it unexpectedly sank before the exercise could take place. This incident occurred in an area facing the disputed Scarborough Shoal, which has been the site of increasing tensions between China and the Philippines. The USS Brattleboro, which participated in crucial battles during World War II, sank at 7:20 AM local time on Monday, April 28, 2025. The ship, designated as a submarine chaser, served in important roles in the Battle of Leyte and the invasion of Okinawa. Despite the ship's age and unsuitability for normal operations, it was selected as the target for the MARSTRIKE exercise. The Philippine and US joint task forces will still achieve their training objectives, as other elements of the exercise were still scheduled to occur. The Philippine military stated that there was no environmental danger from the sinking, as the vessel had been cleaned before being towed out for the exercise. This event highlights the importance of maintaining and preserving historical military assets while also addressing the challenges posed by the potential risks involved in using such assets for military exercises.
```

The entire process takes about `1566.37 seconds` to run when the `Qwen2.5-3B-Instruct` model is ran using [llamacpp](https://github.com/ggml-org/llama.cpp) on Google Collab's A100 GPU. There is an `ipynb` [here](./Awful_News_Llama_A100.ipynb) for automatically deploying `llama.cpp` using the `Qwen2.5-3B-Instruct` model and `cloudflared` to provide the public URL for your `api_base` configuration.